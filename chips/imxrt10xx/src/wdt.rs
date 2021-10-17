use core::cell::Cell;
use kernel::debug;

use kernel::utilities::registers::interfaces::{ReadWriteable, Writeable};
use kernel::utilities::registers::{register_bitfields, register_structs, ReadOnly, ReadWrite};
use kernel::utilities::StaticRef;

register_structs! {
    WdtRegisters {
        // WDOG1
        // Watchdog Control Register
        (0x000 => wcr: ReadWrite<u16, WCR::Register>),
        // wWtchdog Service Register
        (0x002 => wsr: ReadWrite<u16, WSR::Register>),
        // Watchdog Reset Status Register
        (0x004 => wrsr: ReadOnly<u16, WRSR::Register>),
        // Watchdog Interrupt Control Register
        (0x006 => wicr: ReadWrite<u16, WICR::Register>),
        // Watchdog Miscellaneous Control Register
        (0x008 => wmcr: ReadWrite<u16, WMCR::Register>),
        // The end of the struct is marked as follows.
        (0x00A => @END),
    }
}

register_bitfields![u16,
    WCR [
        // Watchdog Time-out Field
        WT OFFSET(8) NUMBITS(8) [],
        // Watchdog Disable for Wait
        WDW OFFSET(7) NUMBITS(1) [],
        // Software Reset Extension
        SRE OFFSET(6) NUMBITS(1) [],
        // WDOG_B assertion
        WDA OFFSET(5) NUMBITS(1) [],
        // Software Reset Signal
        SRS OFFSET(4) NUMBITS(1) [],
        // WDOG_B Time-Out Assertion
        WDT OFFSET(3) NUMBITS(1) [],
        // Watchdog Enable
        WDE OFFSET(2) NUMBITS(1) [],
        // Watchdog DEBUG Enable
        WDBG OFFSET(1) NUMBITS(1) [],
        // Watchdog Low Power
        WDZST OFFSET(0) NUMBITS(1) []
    ],

    WSR [
        // Watchdog Service Register
        WSR OFFSET(0) NUMBITS(16) [
            KEY1 = 0x_5555,
            KEY2 = 0x_AAAA,
        ]
    ],

    WRSR [
        // Power On Reset
        POR OFFSET(4) NUMBITS(1) [],
        // Timeout
        TOUT OFFSET(1) NUMBITS(1) [],
        // Software Reset
        SFTW OFFSET(0) NUMBITS(1) []
    ],

    WICR [
        // Watchdog Timer Interrupt enable bit
        WIE OFFSET(15) NUMBITS(1) [],
        // Watchdog TImer Interrupt Status
        WTIS OFFSET(14) NUMBITS(1) [],
        // Watchdog Interrupt Count Time-out (WICT)
        WICT OFFSET(0) NUMBITS(8) []
    ],

    WMCR [
        // Power Down Enable bit
        PDE OFFSET(0) NUMBITS(1) []
    ]
];

// Page 3187 of imxrt1060
const WDOG1_BASE: StaticRef<WdtRegisters> =
    unsafe { StaticRef::new(0x400B_8000 as *const WdtRegisters) };

pub struct Wdt {
    enabled: Cell<bool>,
    registers: StaticRef<WdtRegisters>,
}

impl Wdt {
    pub const fn new() -> Wdt {
        Wdt {
            enabled: Cell::new(false),
            registers: WDOG1_BASE,
        }
    }

    fn start(&self) {
        self.enabled.set(true);

        self.registers.wmcr.modify(WMCR::PDE::CLEAR);
        self.registers.wcr.modify(WCR::WT.val(0));
        self.registers.wcr.modify(WCR::WDE::SET);
        debug!("finished start");
    }

    fn suspend(&self) {
        self.registers.wcr.modify(WCR::WDZST::SET);
        self.enabled.set(false);
    }

    fn tickle(&self) {
        // if watchdog was suspended, restart it
        if !(self.enabled.get()) {
            self.registers.wcr.modify(WCR::WDZST::CLEAR);
            self.enabled.set(true);
        }
        self.registers.wsr.write(WSR::WSR::KEY1);
        self.registers.wsr.write(WSR::WSR::KEY2);
    }
}

static mut count: u32 = 0;

impl kernel::platform::watchdog::WatchDog for Wdt {
    fn setup(&self) {
        debug!("called setup {}", unsafe { count });
        unsafe { count += 1 };
        self.start(); // Starts with 0.5 seconds
    }

    fn tickle(&self) {
        self.tickle();
    }

    fn suspend(&self) {
        self.suspend();
    }
}
