// AF_XDP UMEM — shared memory region between kernel and user space.
// Adapted from Runbound's xdp/umem.rs for a receive-only path (dnsmark
// sends queries via regular UDP sockets and only receives via AF_XDP).

#![allow(dead_code)]

use std::os::fd::RawFd;
use std::sync::atomic::{fence, Ordering};
use std::{ptr, slice};

use libc::{
    MAP_ANONYMOUS, MAP_FAILED, MAP_POPULATE, MAP_SHARED, PROT_READ, PROT_WRITE,
    mmap, munmap, sysconf, _SC_PAGESIZE,
};

// ── Frame configuration ────────────────────────────────────────────────────

pub const FRAME_SIZE: u32 = 4096;
pub const FRAME_COUNT: u32 = 4096;
pub const RING_SIZE: u32 = 2048;

// ── Kernel constants (from <linux/if_xdp.h>) ──────────────────────────────

pub const SOL_XDP: libc::c_int = 283;
pub const XDP_MMAP_OFFSETS: libc::c_int = 1;
pub const XDP_RX_RING: libc::c_int = 2;
pub const XDP_TX_RING: libc::c_int = 3;
pub const XDP_UMEM_REG: libc::c_int = 4;
pub const XDP_UMEM_FILL_RING: libc::c_int = 5;
pub const XDP_UMEM_COMPLETION_RING: libc::c_int = 6;

pub const XDP_PGOFF_RX_RING: libc::off_t = 0;
pub const XDP_PGOFF_TX_RING: libc::off_t = 0x8000_0000;
pub const XDP_UMEM_PGOFF_FILL_RING: libc::off_t = 0x1_0000_0000;
pub const XDP_UMEM_PGOFF_COMPLETION_RING: libc::off_t = 0x1_8000_0000;

pub const XDP_RING_NEED_WAKEUP: u32 = 1;

pub const XDP_ZEROCOPY: u16 = 1 << 2;
pub const XDP_COPY: u16 = 1 << 1;
pub const XDP_USE_NEED_WAKEUP: u16 = 1 << 3;

#[repr(C)]
pub struct XdpUmemReg {
    pub addr: u64,
    pub len: u64,
    pub chunk_size: u32,
    pub headroom: u32,
    pub flags: u32,
    pub tx_metadata_len: u32,
}

#[repr(C)]
pub struct XdpRingOffsets {
    pub producer: u64,
    pub consumer: u64,
    pub desc: u64,
    pub flags: u64,
}

#[repr(C)]
pub struct XdpMmapOffsets {
    pub rx: XdpRingOffsets,
    pub tx: XdpRingOffsets,
    pub fr: XdpRingOffsets,
    pub cr: XdpRingOffsets,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct XdpDesc {
    pub addr: u64,
    pub len: u32,
    pub options: u32,
}

#[repr(C)]
pub struct SockaddrXdp {
    pub sxdp_family: u16,
    pub sxdp_flags: u16,
    pub sxdp_ifindex: u32,
    pub sxdp_queue_id: u32,
    pub sxdp_shared_umem_fd: u32,
}

// ── Fill / completion ring (u64 UMEM offsets) ─────────────────────────────

pub struct AddrRing {
    _map:     *mut u8,
    _mapsize: usize,
    producer: *mut u32,
    consumer: *mut u32,
    flags:    *mut u32,
    descs:    *mut u64,
    pub size: u32,
    pub mask: u32,
}

unsafe impl Send for AddrRing {}

impl AddrRing {
    pub fn zeroed() -> Self {
        AddrRing {
            _map: ptr::null_mut(), _mapsize: 0,
            producer: ptr::null_mut(), consumer: ptr::null_mut(),
            flags: ptr::null_mut(), descs: ptr::null_mut(),
            size: 0, mask: 0,
        }
    }

    pub fn enqueue_batch(&self, addrs: &[u64]) -> usize {
        let prod = unsafe { ptr::read_volatile(self.producer) };
        let cons = unsafe { ptr::read_volatile(self.consumer) };
        let free = self.size.wrapping_sub(prod.wrapping_sub(cons)) as usize;
        let n = addrs.len().min(free);
        for (i, &a) in addrs[..n].iter().enumerate() {
            let idx = prod.wrapping_add(i as u32) & self.mask;
            unsafe { ptr::write_volatile(self.descs.add(idx as usize), a); }
        }
        fence(Ordering::Release);
        unsafe { ptr::write_volatile(self.producer, prod.wrapping_add(n as u32)); }
        n
    }

    pub fn dequeue_all(&self) -> Vec<u64> {
        fence(Ordering::Acquire);
        let prod = unsafe { ptr::read_volatile(self.producer) };
        let cons = unsafe { ptr::read_volatile(self.consumer) };
        let available = prod.wrapping_sub(cons) as usize;
        let mut out = Vec::with_capacity(available);
        for i in 0..available {
            let idx = cons.wrapping_add(i as u32) & self.mask;
            out.push(unsafe { ptr::read_volatile(self.descs.add(idx as usize)) });
        }
        if available > 0 {
            unsafe { ptr::write_volatile(self.consumer, cons.wrapping_add(available as u32)); }
        }
        out
    }

    pub fn needs_wakeup(&self) -> bool {
        fence(Ordering::Acquire);
        (unsafe { ptr::read_volatile(self.flags) } & XDP_RING_NEED_WAKEUP) != 0
    }
}

// ── RX ring (XdpDesc descriptors) ─────────────────────────────────────────

pub struct DescRing {
    _map:     *mut u8,
    _mapsize: usize,
    producer: *mut u32,
    consumer: *mut u32,
    pub flags: *mut u32,
    descs:    *mut XdpDesc,
    pub size: u32,
    pub mask: u32,
}

unsafe impl Send for DescRing {}

impl DescRing {
    pub fn zeroed() -> Self {
        DescRing {
            _map: ptr::null_mut(), _mapsize: 0,
            producer: ptr::null_mut(), consumer: ptr::null_mut(),
            flags: ptr::null_mut(), descs: ptr::null_mut(),
            size: 0, mask: 0,
        }
    }

    /// Submit descriptors to the TX ring.  Returns number actually enqueued.
    pub fn produce_tx(&self, descs: &[XdpDesc]) -> usize {
        let prod = unsafe { ptr::read_volatile(self.producer) };
        let cons = unsafe { ptr::read_volatile(self.consumer) };
        let free = self.size.wrapping_sub(prod.wrapping_sub(cons)) as usize;
        let n = descs.len().min(free);
        for (i, desc) in descs[..n].iter().enumerate() {
            let idx = prod.wrapping_add(i as u32) & self.mask;
            unsafe { ptr::write_volatile(self.descs.add(idx as usize), *desc); }
        }
        if n > 0 {
            fence(Ordering::Release);
            unsafe { ptr::write_volatile(self.producer, prod.wrapping_add(n as u32)); }
        }
        n
    }

    pub fn consume_rx(&self) -> Vec<XdpDesc> {
        fence(Ordering::Acquire);
        let prod = unsafe { ptr::read_volatile(self.producer) };
        let cons = unsafe { ptr::read_volatile(self.consumer) };
        let available = prod.wrapping_sub(cons) as usize;
        if available == 0 { return Vec::new(); }
        let mut out = Vec::with_capacity(available);
        for i in 0..available {
            let idx = cons.wrapping_add(i as u32) & self.mask;
            out.push(unsafe { ptr::read_volatile(self.descs.add(idx as usize)) });
        }
        unsafe { ptr::write_volatile(self.consumer, cons.wrapping_add(available as u32)); }
        out
    }

    pub fn needs_wakeup(&self) -> bool {
        fence(Ordering::Acquire);
        (unsafe { ptr::read_volatile(self.flags) } & XDP_RING_NEED_WAKEUP) != 0
    }
}

// ── UMEM ──────────────────────────────────────────────────────────────────

pub struct Umem {
    pub area:     *mut u8,
    pub area_len: usize,
    pub fill:     AddrRing,
    pub comp:     AddrRing,
}

unsafe impl Send for Umem {}

impl Umem {
    /// Returns (Umem, tx_pool) where tx_pool contains UMEM frame offsets
    /// for the TX path (frames RING_SIZE..FRAME_COUNT).
    pub unsafe fn new(xsk_fd: RawFd) -> Result<(Self, Vec<u64>), String> {
        let page = sysconf(_SC_PAGESIZE) as usize;
        let area_len = ((FRAME_COUNT * FRAME_SIZE) as usize + page - 1) & !(page - 1);

        let area = mmap(
            ptr::null_mut(),
            area_len,
            PROT_READ | PROT_WRITE,
            MAP_SHARED | MAP_ANONYMOUS | MAP_POPULATE,
            -1,
            0,
        );
        if area == MAP_FAILED {
            return Err(format!("UMEM mmap: {}", std::io::Error::last_os_error()));
        }
        let area = area as *mut u8;

        let reg = XdpUmemReg {
            addr: area as u64,
            len: area_len as u64,
            chunk_size: FRAME_SIZE,
            headroom: 0,
            flags: 0,
            tx_metadata_len: 0,
        };
        let rc = libc::setsockopt(
            xsk_fd, SOL_XDP, XDP_UMEM_REG,
            &reg as *const _ as *const libc::c_void,
            std::mem::size_of::<XdpUmemReg>() as libc::socklen_t,
        );
        if rc != 0 {
            munmap(area as *mut libc::c_void, area_len);
            return Err(format!("XDP_UMEM_REG: {}", std::io::Error::last_os_error()));
        }

        for (opt, sz) in [
            (XDP_UMEM_FILL_RING, RING_SIZE),
            (XDP_UMEM_COMPLETION_RING, RING_SIZE),
        ] {
            let rc = libc::setsockopt(
                xsk_fd, SOL_XDP, opt,
                &sz as *const _ as *const libc::c_void,
                std::mem::size_of::<u32>() as libc::socklen_t,
            );
            if rc != 0 {
                munmap(area as *mut libc::c_void, area_len);
                return Err(format!("setsockopt ring ({opt}): {}", std::io::Error::last_os_error()));
            }
        }

        let offsets = get_mmap_offsets(xsk_fd)?;

        let fill = mmap_addr_ring(xsk_fd, XDP_UMEM_PGOFF_FILL_RING, &offsets.fr, RING_SIZE)
            .inspect_err(|_| { unsafe { munmap(area as *mut libc::c_void, area_len); } })?;
        let comp = mmap_addr_ring(xsk_fd, XDP_UMEM_PGOFF_COMPLETION_RING, &offsets.cr, RING_SIZE)
            .inspect_err(|_| { unsafe { munmap(area as *mut libc::c_void, area_len); } })?;

        // RX fill ring: frames 0..RING_SIZE
        let rx_addrs: Vec<u64> = (0..RING_SIZE).map(|i| (i * FRAME_SIZE) as u64).collect();
        fill.enqueue_batch(&rx_addrs);
        // TX pool: remaining frames RING_SIZE..FRAME_COUNT (already mmap'd)
        let tx_pool: Vec<u64> = (RING_SIZE..FRAME_COUNT).map(|i| (i * FRAME_SIZE) as u64).collect();

        Ok((Umem { area, area_len, fill, comp }, tx_pool))
    }

    pub unsafe fn frame(&self, offset: u64, len: usize) -> &[u8] {
        debug_assert!((offset as usize).saturating_add(len) <= self.area_len);
        slice::from_raw_parts(self.area.add(offset as usize), len)
    }

    /// Raw mutable pointer into UMEM at `offset`. Used by XDP TX senders to
    /// write Ethernet frames directly into shared memory.
    pub fn ptr_at(&self, offset: u64) -> *mut u8 {
        unsafe { self.area.add(offset as usize) }
    }
}

impl Drop for Umem {
    fn drop(&mut self) {
        unsafe { munmap(self.area as *mut libc::c_void, self.area_len); }
    }
}

// ── Ring setup helpers ─────────────────────────────────────────────────────

pub unsafe fn get_mmap_offsets(fd: RawFd) -> Result<XdpMmapOffsets, String> {
    let mut offsets = std::mem::MaybeUninit::<XdpMmapOffsets>::uninit();
    let mut optlen = std::mem::size_of::<XdpMmapOffsets>() as libc::socklen_t;
    let rc = libc::getsockopt(
        fd, SOL_XDP, XDP_MMAP_OFFSETS,
        offsets.as_mut_ptr() as *mut libc::c_void,
        &mut optlen,
    );
    if rc != 0 {
        return Err(format!("XDP_MMAP_OFFSETS: {}", std::io::Error::last_os_error()));
    }
    Ok(offsets.assume_init())
}

pub unsafe fn get_rx_tx_offsets(fd: RawFd) -> Result<(XdpRingOffsets, XdpRingOffsets), String> {
    let o = get_mmap_offsets(fd)?;
    Ok((o.rx, o.tx))
}

unsafe fn mmap_addr_ring(
    fd: RawFd,
    pgoff: libc::off_t,
    off: &XdpRingOffsets,
    size: u32,
) -> Result<AddrRing, String> {
    let mapsize = off.desc as usize + size as usize * std::mem::size_of::<u64>();
    let map = mmap(
        ptr::null_mut(), mapsize,
        PROT_READ | PROT_WRITE,
        MAP_SHARED | MAP_POPULATE,
        fd, pgoff,
    );
    if map == MAP_FAILED {
        return Err(format!("addr ring mmap (pgoff={pgoff:#x}): {}", std::io::Error::last_os_error()));
    }
    let map = map as *mut u8;
    Ok(AddrRing {
        _map: map, _mapsize: mapsize,
        producer: map.add(off.producer as usize) as *mut u32,
        consumer: map.add(off.consumer as usize) as *mut u32,
        flags:    map.add(off.flags    as usize) as *mut u32,
        descs:    map.add(off.desc     as usize) as *mut u64,
        size, mask: size - 1,
    })
}

pub unsafe fn mmap_desc_ring(
    fd: RawFd,
    pgoff: libc::off_t,
    off: &XdpRingOffsets,
    size: u32,
) -> Result<DescRing, String> {
    let mapsize = off.desc as usize + size as usize * std::mem::size_of::<XdpDesc>();
    let map = mmap(
        ptr::null_mut(), mapsize,
        PROT_READ | PROT_WRITE,
        MAP_SHARED | MAP_POPULATE,
        fd, pgoff,
    );
    if map == MAP_FAILED {
        return Err(format!("desc ring mmap (pgoff={pgoff:#x}): {}", std::io::Error::last_os_error()));
    }
    let map = map as *mut u8;
    Ok(DescRing {
        _map: map, _mapsize: mapsize,
        producer: map.add(off.producer as usize) as *mut u32,
        consumer: map.add(off.consumer as usize) as *mut u32,
        flags:    map.add(off.flags    as usize) as *mut u32,
        descs:    map.add(off.desc     as usize) as *mut XdpDesc,
        size, mask: size - 1,
    })
}
