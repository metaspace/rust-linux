pub mod regs_rt;

#[doc = "NVMe controller"]
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Nvme {
    ptr: *mut u8,
}
unsafe impl Send for Nvme {}
unsafe impl Sync for Nvme {}
impl Nvme {
    #[inline(always)]
    pub const unsafe fn from_ptr(ptr: *mut ()) -> Self {
        Self { ptr: ptr as _ }
    }
    #[inline(always)]
    pub const fn as_ptr(&self) -> *mut () {
        self.ptr as _
    }
    #[doc = "Controller Capabilities"]
    #[inline(always)]
    pub const fn cap(self) -> regs_rt::Reg<regs::Cap, regs_rt::R> {
        unsafe { regs_rt::Reg::from_ptr(self.ptr.add(0usize) as _) }
    }
    #[doc = "Controller Status"]
    #[inline(always)]
    pub const fn csts(self) -> regs_rt::Reg<regs::Csts, regs_rt::R> {
        unsafe { regs_rt::Reg::from_ptr(self.ptr.add(28usize) as _) }
    }
}
pub mod regs {
    #[doc = "Controller Capabilities"]
    #[repr(transparent)]
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct Cap(pub u64);
    impl Cap {
        #[doc = "Maximum Queue Entries Supported"]
        #[inline(always)]
        pub const fn mqes(&self) -> u16 {
            let val = (self.0 >> 0usize) & 0xffff;
            val as u16
        }
        #[doc = "Maximum Queue Entries Supported"]
        #[inline(always)]
        pub fn set_mqes(&mut self, val: u16) {
            self.0 = (self.0 & !(0xffff << 0usize)) | (((val as u64) & 0xffff) << 0usize);
        }
        #[doc = "Continuous Queues Required"]
        #[inline(always)]
        pub const fn cqr(&self) -> bool {
            let val = (self.0 >> 16usize) & 0x01;
            val != 0
        }
        #[doc = "Continuous Queues Required"]
        #[inline(always)]
        pub fn set_cqr(&mut self, val: bool) {
            self.0 = (self.0 & !(0x01 << 16usize)) | (((val as u64) & 0x01) << 16usize);
        }
        #[doc = "Doorbell stride"]
        #[inline(always)]
        pub const fn dstrd(&self) -> u8 {
            let val = (self.0 >> 32usize) & 0x0f;
            val as u8
        }
        #[doc = "Doorbell stride"]
        #[inline(always)]
        pub fn set_dstrd(&mut self, val: u8) {
            self.0 = (self.0 & !(0x0f << 32usize)) | (((val as u64) & 0x0f) << 32usize);
        }
    }
    impl Default for Cap {
        #[inline(always)]
        fn default() -> Cap {
            Cap(0)
        }
    }
    #[doc = "Controller Status"]
    #[repr(transparent)]
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct Csts(pub u64);
    impl Csts {
        #[doc = "Ready"]
        #[inline(always)]
        pub const fn rdy(&self) -> bool {
            let val = (self.0 >> 0usize) & 0x01;
            val != 0
        }
        #[doc = "Ready"]
        #[inline(always)]
        pub fn set_rdy(&mut self, val: bool) {
            self.0 = (self.0 & !(0x01 << 0usize)) | (((val as u64) & 0x01) << 0usize);
        }
        #[doc = "Controller Fatal Status"]
        #[inline(always)]
        pub const fn cfs(&self) -> bool {
            let val = (self.0 >> 1usize) & 0x01;
            val != 0
        }
        #[doc = "Controller Fatal Status"]
        #[inline(always)]
        pub fn set_cfs(&mut self, val: bool) {
            self.0 = (self.0 & !(0x01 << 1usize)) | (((val as u64) & 0x01) << 1usize);
        }
        #[doc = "Controller Shutdownw Status"]
        #[inline(always)]
        pub const fn shst(&self) -> super::vals::Shst {
            let val = (self.0 >> 2usize) & 0x03;
            super::vals::Shst::from_bits(val as u8)
        }
        #[doc = "Controller Shutdownw Status"]
        #[inline(always)]
        pub fn set_shst(&mut self, val: super::vals::Shst) {
            self.0 = (self.0 & !(0x03 << 2usize)) | (((val.to_bits() as u64) & 0x03) << 2usize);
        }
        #[doc = "NVMe subsystem reset occured"]
        #[inline(always)]
        pub const fn nssro(&self) -> bool {
            let val = (self.0 >> 4usize) & 0x01;
            val != 0
        }
        #[doc = "NVMe subsystem reset occured"]
        #[inline(always)]
        pub fn set_nssro(&mut self, val: bool) {
            self.0 = (self.0 & !(0x01 << 4usize)) | (((val as u64) & 0x01) << 4usize);
        }
        #[doc = "Processing Paused"]
        #[inline(always)]
        pub const fn pp(&self) -> bool {
            let val = (self.0 >> 5usize) & 0x01;
            val != 0
        }
        #[doc = "Processing Paused"]
        #[inline(always)]
        pub fn set_pp(&mut self, val: bool) {
            self.0 = (self.0 & !(0x01 << 5usize)) | (((val as u64) & 0x01) << 5usize);
        }
        #[doc = "Shutdown Type"]
        #[inline(always)]
        pub const fn st(&self) -> bool {
            let val = (self.0 >> 6usize) & 0x01;
            val != 0
        }
        #[doc = "Shutdown Type"]
        #[inline(always)]
        pub fn set_st(&mut self, val: bool) {
            self.0 = (self.0 & !(0x01 << 6usize)) | (((val as u64) & 0x01) << 6usize);
        }
    }
    impl Default for Csts {
        #[inline(always)]
        fn default() -> Csts {
            Csts(0)
        }
    }
}
pub mod vals {
    #[repr(u8)]
    #[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
    pub enum Shst {
        #[doc = "Normal Operation (no shutdown requested)"]
        NORMALOPERATION = 0,
        #[doc = "Shutdown in progress"]
        SHUTDOWNOCCURRING = 0x01,
        #[doc = "Shutdown is complete"]
        SHUTDOWNCOMPLETE = 0x02,
        _RESERVED_3 = 0x03,
    }
    impl Shst {
        #[inline(always)]
        pub const fn from_bits(val: u8) -> Shst {
            unsafe { core::mem::transmute(val & 0x03) }
        }
        #[inline(always)]
        pub const fn to_bits(self) -> u8 {
            unsafe { core::mem::transmute(self) }
        }
    }
    impl From<u8> for Shst {
        #[inline(always)]
        fn from(val: u8) -> Shst {
            Shst::from_bits(val)
        }
    }
    impl From<Shst> for u8 {
        #[inline(always)]
        fn from(val: Shst) -> u8 {
            Shst::to_bits(val)
        }
    }
}
