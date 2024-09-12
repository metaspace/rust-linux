// SPDX-License-Identifier: GPL-2.0

//! Rust hrtimer sample.

use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;
use kernel::{
    hrtimer::{Timer, TimerCallback, TimerCallbackContext, TimerPointer, TimerRestart},
    impl_has_timer,
    prelude::*,
    sync::Arc,
};

module! {
    type: RustMinimal,
    name: "hrtimer",
    author: "Rust for Linux Contributors",
    description: "Rust hrtimer sample",
    license: "GPL",
}

struct RustMinimal {}

#[pin_data]
struct PinMutIntrusiveTimer {
    #[pin]
    timer: Timer<Self>,
    // TODO: Change to CondVar
    flag: Arc<AtomicBool>,
}

impl PinMutIntrusiveTimer
{
    fn new() -> impl PinInit<Self, kernel::error::Error>
    {
        try_pin_init!(Self {
            timer <- Timer::new::<Pin<&mut _>>(),
            flag: Arc::new(AtomicBool::new(false), kernel::alloc::flags::GFP_KERNEL)?,
        })
    }
}

impl TimerCallback for PinMutIntrusiveTimer
{
    type CallbackTarget<'a> =  Pin<&'a mut Self>;

    fn run(this: Self::CallbackTarget<'_>, _ctx: TimerCallbackContext<'_, Self>) -> TimerRestart {
        pr_info!("Timer called\n");
        this.flag.store(true, Ordering::Relaxed);
        TimerRestart::NoRestart
    }
}

impl_has_timer! {
    impl HasTimer<Self> for PinMutIntrusiveTimer { self.timer }
}

fn stack_timer() -> Result<()> {
    use kernel::stack_try_pin_init;

    pr_info!("Timer on the stack\n");

    stack_try_pin_init!( let has_timer =? PinMutIntrusiveTimer::new() );
    let flag_handle = has_timer.flag.clone();
    let _handle = has_timer.as_mut().schedule(200_000_000);

    while !flag_handle.load(Ordering::Relaxed) {
        core::hint::spin_loop()
    }

    pr_info!("Flag raised\n");
    Ok(())
}


#[pin_data]
struct ArcIntrusiveTimer {
    #[pin]
    timer: Timer<Self>,
    // TODO: Change to CondVar
    flag: AtomicBool,
}

impl ArcIntrusiveTimer
{
    fn new() -> impl PinInit<Self, kernel::error::Error>
    {
        try_pin_init!(Self {
            timer <- Timer::new::<Arc<_>>(),
            flag: AtomicBool::new(false),
        })
    }
}

impl TimerCallback for ArcIntrusiveTimer
{
    type CallbackTarget<'a> =  Arc<Self>;

    fn run(this: Self::CallbackTarget<'_>, _ctx: TimerCallbackContext<'_, Self>) -> TimerRestart {
        pr_info!("Timer called\n");
        this.flag.store(true, Ordering::Relaxed);
        TimerRestart::NoRestart
    }
}

impl_has_timer! {
    impl HasTimer<Self> for ArcIntrusiveTimer { self.timer }
}

fn arc_timer() -> Result<()> {
    pr_info!("Timer on the heap in Arc\n");

    let has_timer = Arc::pin_init(ArcIntrusiveTimer::new(), GFP_KERNEL)?;
    let _handle = has_timer.clone().schedule(200_000_000);
    while !has_timer.flag.load(Ordering::Relaxed) {
        core::hint::spin_loop()
    }

    pr_info!("Flag raised\n");
    Ok(())
}

impl kernel::Module for RustMinimal {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust hrtimer sample (init)\n");
        pr_info!("Am I built-in? {}\n", !cfg!(MODULE));

        stack_timer()?;
        arc_timer()?;

        Ok(RustMinimal {})
    }
}

impl Drop for RustMinimal {
    fn drop(&mut self) {
        pr_info!("Rust hrtimer sample (exit)\n");
    }
}
