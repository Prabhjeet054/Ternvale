use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{GedRegs, Pl061, PowerButton, POWER_GPIO_PIN};
use crate::mmio::MmioDevice;

#[test]
fn press_lowers_gpio_pin_and_sets_ged_bit() {
    let gpio_irq = Arc::new(AtomicBool::new(false));
    let ged_irq = Arc::new(AtomicBool::new(false));
    let g = Arc::clone(&gpio_irq);
    let e = Arc::clone(&ged_irq);
    let button = PowerButton::new(
        Arc::new(move |level| g.store(level, Ordering::SeqCst)),
        Arc::new(move |level| e.store(level, Ordering::SeqCst)),
    );
    button.press();
    let mut gpio = Pl061::new(Arc::clone(&button));
    let mut ged = GedRegs::new(Arc::clone(&button));
    assert_eq!(gpio.read(0, 4) & (1 << POWER_GPIO_PIN), 0);
    assert_eq!(ged.read(0, 4), 1);
    assert!(ged_irq.load(Ordering::SeqCst));
    gpio.write(super::IE, 4, 1 << POWER_GPIO_PIN);
    assert!(gpio_irq.load(Ordering::SeqCst) || gpio.read(super::MIS, 4) != 0);
    ged.write(0, 4, 1);
    assert_eq!(ged.read(0, 4), 0);
    assert!(!ged_irq.load(Ordering::SeqCst));
}

#[test]
fn pl061_ids_match_the_primecell() {
    let button = PowerButton::new(Arc::new(|_| {}), Arc::new(|_| {}));
    let mut gpio = Pl061::new(button);
    assert_eq!(gpio.read(0xfe0, 1), 0x10);
    assert_eq!(gpio.read(0xff0, 1), 0x0d);
    assert_eq!(gpio.read(0xffc, 1), 0xb1);
}
