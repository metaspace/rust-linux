// SPDX-License-Identifier: GPL-2.0

//! This is a reimplementation of the network block device driver (nbd.c)
//! in Rust.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use kernel::{
    bindings,
    block::{
        self,
        bio::Segment,
        mq::{self, GenDisk, Operations, TagSet},
    },
    error,
    impl_has_work,
    net::socket::{
        flags::{FlagSet, ReceiveFlag, SendFlag},
        ShutdownCmd,
        Socket,
    },
    new_condvar, new_mutex, new_work, pr_info, pr_err,
    prelude::*,
    stack_pin_init,
    sync::{Arc, ArcBorrow, CondVar, Mutex},
    types::ForeignOwnable,
    uapi,
    workqueue::{self, Work, WorkItem}
};

module! {
    type: NbdModule,
    name: "nbd_rust",
    author: "",
    license: "GPL",
    params: {
        nbds_max: u8 {
            default: 16,
            permissions: 0,
            description: "Number of network block devices to initialize",
        },
    },
}

const REPLY_SIZE: usize = 16;
const REQUEST_SIZE: usize = 28;

fn parse_reply(bytes: &[u8; REPLY_SIZE]) -> Result<(Result, u32, u32)> {
    let (magic_bytes, bytes) = bytes.split_at(4);
    let (error_bytes, bytes) = bytes.split_at(4);
    let (hctx_bytes, bytes) = bytes.split_at(4);
    let (tag_bytes, _) = bytes.split_at(4);
    if u32::from_be_bytes(magic_bytes.try_into().unwrap()) != uapi::NBD_REPLY_MAGIC {
        pr_err!("Invalid reply magic\n");
        return Err(error::code::EINVAL);
    }

    let reply_error = u32::from_be_bytes(error_bytes.try_into().unwrap());
    Ok((
        if reply_error == 0 {
            Ok(())
        } else {
            pr_err!("Remote error {reply_error}\n");
            Err(error::code::EIO)
        },
        u32::from_be_bytes(hctx_bytes.try_into().unwrap()),
        u32::from_be_bytes(tag_bytes.try_into().unwrap()),
    ))
}

// for some reason, the `tag` in `struct request` is a signed integer, so we use i32 here
// for the tag.
fn build_request(index: u32, tag: i32, cmd: bindings::req_op, from: u64, len: u32) -> Result<Vec<u8>> {
    let mut bytes = Vec::try_with_capacity(REQUEST_SIZE)?;
    bytes.try_extend_from_slice(&uapi::NBD_REQUEST_MAGIC.to_be_bytes())?;
    let nbd_cmd = match cmd {
        bindings::req_op_REQ_OP_WRITE => Ok(uapi::NBD_CMD_WRITE),
        bindings::req_op_REQ_OP_READ => Ok(uapi::NBD_CMD_READ),
        bindings::req_op_REQ_OP_FLUSH => Ok(uapi::NBD_CMD_FLUSH),
        bindings::req_op_REQ_OP_DISCARD => Ok(uapi::NBD_CMD_TRIM),
        _ => Err(error::code::ENOTSUPP),
    }?;
    bytes.try_extend_from_slice(&nbd_cmd.to_be_bytes())?;
    bytes.try_extend_from_slice(&index.to_be_bytes())?;
    bytes.try_extend_from_slice(&tag.to_be_bytes())?;
    bytes.try_extend_from_slice(&from.to_be_bytes())?;
    bytes.try_extend_from_slice(&len.to_be_bytes())?;
    Ok(bytes)
}

struct NbdConfig {
    size: u64,
    blk_size: u32,
    blk_size_bits: u32,
}

impl Default for NbdConfig {
    fn default() -> Self {
        NbdConfig {
            size: 0,
            blk_size: 1024,
            blk_size_bits: 10,
        }
    }
}

#[pin_data]
struct NbdSocket {
    queue_data: Arc<NbdQueue>,
    socket: Socket,
    #[pin]
    work: Work<Self>,
}

struct NbdDisk {
    gendisk: Option<GenDisk<NbdQueue>>,
}

#[pin_data]
struct NbdRequestData {
    result: Result,
    socket: Option<Arc<NbdSocket>>,
}

#[pin_data]
struct NbdQueue {
    disk: Arc<Mutex<NbdDisk>>,
    #[pin]
    sockets: Mutex<Vec<Arc<NbdSocket>>>,
    #[pin]
    sockets_removed: CondVar,
    #[pin]
    config: Mutex<NbdConfig>,
    disconnected: AtomicBool,
    live_connections: AtomicU32,
}

impl_has_work! {
    impl HasWork<Self> for NbdSocket { self.work }
}

impl NbdSocket {
    fn try_new(queue_data: Arc<NbdQueue>, fd: i32) -> Result<Arc<Self>> {
        Ok(Arc::pin_init(try_pin_init!(NbdSocket {
            queue_data,
            socket: Socket::fd_lookup(fd)?,
            work <- new_work!("NbdSocket::work"),
        }))?)
    }

    fn shutdown(&self) {
        if let Err(e) = self.socket.shutdown(ShutdownCmd::Both) {
            pr_err!("Failed to shut down socket: {e:?}\n");
        }
    }

    fn receive_message(&self) -> Result {
        let mut bytes: [u8; REPLY_SIZE] = [0; REPLY_SIZE];
        let len = self.socket.receive(
            &mut bytes,
            FlagSet::<ReceiveFlag>::from(ReceiveFlag::WaitAll)
        )?;
        if len < REPLY_SIZE {
            return Err(error::code::EPIPE)
        }
        let (result, hctx_idx, tag) = parse_reply(&bytes)?;
        pr_info!("got {:?} for [{}:{}]\n", result, hctx_idx, tag);
        self.queue_data.process_reply(&self, result, hctx_idx, tag);
        Ok(())
    }

    fn send_message(&self, index: u32, tag: i32, cmd: bindings::req_op, from: usize, len: u32) -> Result {
        let mut flags = FlagSet::<SendFlag>::empty();
        if cmd == bindings::req_op_REQ_OP_WRITE {
            flags.insert(SendFlag::More);
        }
        pr_info!(
            "request [{}:{}] for {}+{}, type={}, flags={}\n",
            index,
            tag,
            from,
            len,
            cmd,
            flags.value()
        );
        let header = build_request(index, tag, cmd, from.try_into().or(Err(error::code::EINVAL))?, len)?;
        self.socket.send(&header, flags)?;
        Ok(())
    }

    fn send_segment(&self, segment: &Segment<'_>, more: bool) -> Result {
        let mut flags = FlagSet::<SendFlag>::empty();
        if more {
            flags.insert(SendFlag::More);
        }
        stack_pin_init!(let iov_iter = segment.iov_iter(true));
        self.socket.send_iov(&iov_iter, flags)?;
        Ok(())
    }

    fn receive_segment(&self, segment: &Segment<'_>) -> Result {
        let flags = FlagSet::<ReceiveFlag>::from(ReceiveFlag::WaitAll);
        stack_pin_init!(let iov_iter = segment.iov_iter(false));
        self.socket.receive_iov(&iov_iter, flags)?;
        Ok(())
    }

    fn send_disconnect(&self) -> Result {
        let mut bytes: [u8; REQUEST_SIZE] = [0; REQUEST_SIZE];
        bytes[0..4].copy_from_slice(&uapi::NBD_REQUEST_MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&uapi::NBD_CMD_DISC.to_be_bytes());
        self.socket.send(&bytes, FlagSet::<SendFlag>::empty())?;
        Ok(())
    }
}

impl WorkItem for NbdSocket {
    type Pointer = Arc<Self>;

    fn run(this: Self::Pointer) {
        loop {
            match this.receive_message() {
                Ok(_) => continue,
                Err(e) => {
                    if !this.queue_data.disconnected.load(Ordering::Relaxed) {
                        pr_err!("Failed to receive reply: {e:?}\n");
                    }
                    this.queue_data.socket_dead();
                    break;
                }
            }
        }
    }
}

impl NbdQueue {
    fn add_socket(self: ArcBorrow<'_, Self>, fd: u64) -> Result {
        self.sockets
            .lock()
            .try_push(NbdSocket::try_new(Arc::<Self>::from(self), fd as i32)?)?;
        self.live_connections.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn reset(&self) {
        let disk = self.disk.lock();
        let gendisk = disk.gendisk.as_ref().unwrap();
        gendisk.set_capacity_and_notify(0);
    }

    fn socket_dead(&self) {
        if self.live_connections.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.sockets_removed.notify_all();
        }
    }

    fn process_reply(&self, socket: &NbdSocket, mut result: Result, hctx_idx: u32, tag: u32) {
        let tagset: Arc<TagSet<NbdQueue>> = {
            let disk = self.disk.lock();
            disk.gendisk.as_ref().unwrap().tagset().into()
        };
        // FIXME: This is probably not very safe if the callbacks in mq::Operations
        // access the request without locking the disk mutex. Should probably
        // be fixed in the block device abstractions.
        match tagset.tag_to_rq(hctx_idx, tag) {
            None => pr_err!("Cannot find the request for this reply\n"),
            Some(req) => {
                if result.is_ok() && req.command() == bindings::req_op_REQ_OP_READ {
                    for bio in req.bio_iter() {
                        for segment in bio.segment_iter() {
                            if let Err(e) = socket.receive_segment(&segment) {
                                pr_err!("Failed to receive a segment: {:?}\n", e);
                                result = Err(error::code::EIO);
                                break;
                            }
                        }
                    }
                }
                {
                    let mut data = req.pdu().lock();
                    if data.result.is_err() {
                        return;
                    }
                    data.result = result;
                }
                req.complete();
            }
        }
    }
}

#[vtable]
impl block::Operations for NbdQueue {
    type QueueData = Arc<Self>;

    fn ioctl(
        queue_data: ArcBorrow<'_, NbdQueue>,
        _mode: bindings::blk_mode_t,
        cmd: u32,
        arg: u64,
    ) -> Result<i32> {
        match cmd {
            uapi::NBD_DISCONNECT => {
                queue_data.disconnected.store(true, Ordering::Relaxed);
                let sockets = queue_data.sockets.lock();
                for socket in &*sockets {
                    if let Err(e) = socket.send_disconnect() {
                        pr_err!("Disconnect failed: {e:?}\n");
                    }
                }
                Ok(0)
            }
            uapi::NBD_CLEAR_SOCK => {
                {
                    let mut sockets = queue_data.sockets.lock();
                    for socket in &*sockets {
                        socket.shutdown();
                    }
                    sockets.clear();
                }
                queue_data.live_connections.store(0, Ordering::Relaxed);
                queue_data.sockets_removed.notify_all();
                Ok(0)
            }
            uapi::NBD_SET_SOCK => {
                queue_data.add_socket(arg)?;
                Ok(0)
            }
            uapi::NBD_SET_SIZE => {
                let mut cfg = queue_data.config.lock();
                cfg.size = arg;
                let disk = queue_data.disk.lock();
                let gendisk = disk.gendisk.as_ref().unwrap();
                gendisk.set_capacity_and_notify(arg >> 9);
                Ok(0)
            }
            uapi::NBD_SET_BLKSIZE => {
                let mut cfg = queue_data.config.lock();
                if let Ok(arg32) = u32::try_from(arg) {
                    cfg.blk_size = if arg32 == 0 {
                        NbdConfig::default().blk_size
                    } else {
                        arg32
                    };
                    cfg.blk_size_bits = cfg.blk_size.ilog2();

                    let disk = queue_data.disk.lock();
                    let gendisk = disk.gendisk.as_ref().unwrap();
                    gendisk.set_queue_physical_block_size(cfg.blk_size as u32);
                    gendisk.set_queue_logical_block_size(cfg.blk_size as u32);
                    Ok(0)
                } else {
                    Err(error::code::EINVAL)
                }
            }
            uapi::NBD_SET_SIZE_BLOCKS => {
                let mut cfg = queue_data.config.lock();
                if let Some(new_size) = arg.checked_shl(cfg.blk_size_bits) {
                    cfg.size = new_size;
                    let disk = queue_data.disk.lock();
                    let gendisk = disk.gendisk.as_ref().unwrap();
                    gendisk.set_capacity_and_notify(new_size >> 9);
                    Ok(0)
                } else {
                    // overflow
                    Err(error::code::EINVAL)
                }
            }
            uapi::NBD_DO_IT => {
                let mut sockets = queue_data.sockets.lock();
                for socket in &*sockets {
                    let _ = workqueue::system_unbound().enqueue(socket.clone());
                }
                let ret = loop {
                    if queue_data.sockets_removed.wait_interruptible(&mut sockets) {
                        sockets.clear();
                        queue_data.live_connections.store(0, Ordering::Relaxed);
                        break Err(error::code::ERESTARTSYS)
                    } else if queue_data.live_connections.load(Ordering::Relaxed) == 0 {
                        break match queue_data.disconnected.load(Ordering::Relaxed) {
                            true => Ok(0),
                            false => Err(error::code::EIO),
                        }
                    }
                };
                queue_data.reset();
                ret
            }
            uapi::NBD_SET_FLAGS => {
                let disk = queue_data.disk.lock();
                let gendisk = disk.gendisk.as_ref().unwrap();
                let flags = arg as u32;
                if (flags & uapi::NBD_FLAG_SEND_FLUSH) != 0 {
                    if (flags & uapi::NBD_FLAG_SEND_FUA) != 0 {
                        gendisk.set_queue_write_cache(true, true);
                    } else {
                        gendisk.set_queue_write_cache(true, false);
                    }
                } else {
                    gendisk.set_queue_write_cache(false, false);
                }
                Ok(0)
            }
            _ => Err(error::code::ENOTTY),
        }
    }

    fn compat_ioctl(
        queue_data: ArcBorrow<'_, NbdQueue>,
        mode: bindings::blk_mode_t,
        cmd: u32,
        arg: u64,
    ) -> Result<i32> {
        Self::ioctl(queue_data, mode, cmd, arg)
    }
}

#[pin_data]
struct NbdContext {
    idx: u32,
}

#[vtable]
impl Operations for NbdQueue {
    type RequestData = Mutex<NbdRequestData>;
    type RequestDataInit = impl PinInit<Mutex<NbdRequestData>>;
    type HwData = Box<NbdContext>;
    type TagSetData = ();

    fn new_request_data(_tagset_data: ()) -> Self::RequestDataInit {
        new_mutex!(NbdRequestData {
            socket: None,
            result: Ok(())
        })
    }

    #[inline(always)]
    fn queue_rq(
        hw_data: &NbdContext,
        queue_data: ArcBorrow<'_, NbdQueue>,
        rq: mq::Request<Self>,
        _is_last: bool,
    ) -> Result {
        let cmd = rq.command();
        let tag = rq.tag();
        let socket = queue_data
            .sockets
            .lock()
            .get(hw_data.idx as usize)
            .ok_or(error::code::EPIPE)?
            .clone();
        socket.send_message(hw_data.idx, tag, cmd, rq.sector() << 9, rq.payload_bytes())?;

        rq.data().lock().socket = Some(socket.clone());
        rq.start();

        if cmd == bindings::req_op_REQ_OP_WRITE {
            let mut bio_it = rq.bio_iter().peekable();
            while let Some(bio) = bio_it.next() {
                let mut seg_it = bio.segment_iter().peekable();
                let last_bio = bio_it.peek().is_none();
                while let Some(segment) = seg_it.next() {
                    let last_seg = seg_it.peek().is_none();
                    socket.send_segment(&segment, !last_seg || !last_bio)?;
                }
            }
        }

        Ok(())
    }

    fn commit_rqs(
        _hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        _queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
    ) {
    }

    fn timeout(rq: mq::Request<Self>) -> bindings::blk_eh_timer_return {
        pr_err!("Request timed out\n");
        {
            let data_mutex = rq.data();
            let mut data = data_mutex.lock();
            data.result = Err(error::code::EIO);
            data.socket.as_ref().unwrap().shutdown();
        }
        rq.complete();
        bindings::blk_eh_timer_return_BLK_EH_DONE
    }

    fn complete(rq: mq::Request<Self>) {
        let result = rq.data().lock().result.clone();
        rq.end(result);
    }

    fn init_hctx(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
        hctx_idx: u32,
    ) -> Result<Self::HwData> {
        Box::try_init(NbdContext {
            idx: hctx_idx,
        })
    }
}

struct NbdModule {
    disks: Vec<Arc<Mutex<NbdDisk>>>,
}

fn add_disk(index: u8, disk: Arc<Mutex<NbdDisk>>) -> Result<GenDisk<NbdQueue>> {
    let tagset = TagSet::try_new(1, (), 128, 1)?;
    let queue_data = Arc::pin_init(try_pin_init!(NbdQueue {
        disk,
        sockets <- new_mutex!(Vec::new(), "nbd:sockets"),
        sockets_removed <- new_condvar!(),
        config <- new_mutex!(NbdConfig::default(), "nbd:config"),
        disconnected: AtomicBool::new(false),
        live_connections: AtomicU32::new(0),
    }))?;
    let disk = GenDisk::try_new(tagset, queue_data)?;
    disk.set_name(format_args!("nbd{}", index))?;
    disk.set_rotational(false);
    disk.add()?;
    Ok(disk)
}

impl kernel::Module for NbdModule {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("module loaded!\n");

        let num_devs = *nbds_max.read();
        let mut disks = Vec::try_with_capacity(num_devs as usize)?;

        for index in 0..num_devs {
            let disk = Arc::pin_init(new_mutex!(NbdDisk {
                gendisk: None,
            }, "nbd:disk"))?;
            disk.lock().gendisk = Some(add_disk(index, disk.clone())?);
            disks.try_push(disk)?;
        }

        Ok(Self { disks })
    }
}

impl Drop for NbdModule {
    fn drop(&mut self) {
        pr_info!("unloading module\n");

        // break Arc loops to destroy disks
        for disk in &self.disks {
            disk.lock().gendisk = None;
        }
    }
}
