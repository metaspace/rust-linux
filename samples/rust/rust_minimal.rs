// SPDX-License-Identifier: GPL-2.0

//! Rust minimal sample.

use kernel::prelude::*;

module! {
    type: RustMinimal,
    name: "rust_minimal",
    author: "Rust for Linux Contributors",
    description: "Rust minimal sample",
    license: "GPL",
}

struct RustMinimal {
    numbers: Vec<i32>,
}

impl kernel::Module for RustMinimal {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust minimal sample (init)\n");
        pr_info!("Am I built-in? {}\n", !cfg!(MODULE));

        let mut numbers = Vec::new();
        numbers.push(72, GFP_KERNEL)?;
        numbers.push(108, GFP_KERNEL)?;
        numbers.push(200, GFP_KERNEL)?;

        {
            use core::sync::atomic::AtomicBool;
            use core::sync::atomic::Ordering;
            use kernel::{
                alloc::flags,
                hrtimer::{Timer, TimerCallback, TimerCallbackContext, TimerPointer},
                impl_has_timer,
                prelude::*,
                stack_pin_init,
                sync::Arc,
            };

            #[pin_data]
            struct IntrusiveTimer {
                #[pin]
                timer: Timer<Self>,
                // TODO: Change to CondVar
                flag: AtomicBool,
            }

            impl IntrusiveTimer {
                fn new() -> impl PinInit<Self> {
                    pin_init!(Self {
                        timer <- Timer::new(),
                        flag: AtomicBool::new(false),
                    })
                }
            }

            impl TimerCallback for IntrusiveTimer {

                fn run(&self, _ctx: TimerCallbackContext<'_, Self>) {
                    pr_info!("Timer called\n");
                    self.flag.store(true, Ordering::Relaxed);
                }
            }

            impl_has_timer! {
                impl HasTimer<Self> for IntrusiveTimer { self.timer }
            }

            let has_timer = Arc::pin_init(IntrusiveTimer::new(), GFP_KERNEL)?;
            let _handle = has_timer.clone().schedule(200_000_000);
            while !has_timer.flag.load(Ordering::Relaxed) {
                core::hint::spin_loop()
            }

            pr_info!("Flag raised\n");
        }

        Ok(RustMinimal { numbers })
    }
}

impl Drop for RustMinimal {
    fn drop(&mut self) {
        pr_info!("My numbers are {:?}\n", self.numbers);
        pr_info!("Rust minimal sample (exit)\n");
    }
}
