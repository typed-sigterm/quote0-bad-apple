//! Minimal UC8251D e-paper display driver (152x296, 1bpp, full refresh).
//!
//! Ported instruction-for-instruction from the quote0-desktop-status C driver
//! (`firmware/components/uc8251d/uc8251d_minimal.c`), which is known-good on
//! the MindReset Quote/0 hardware.
//!
//! Frame format: 5616 bytes, row-major, MSB first, 1 = white, 0 = black.

use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::peripherals::{GPIO3, GPIO4, GPIO5, GPIO6, GPIO7, GPIO10, GPIO20, SPI2};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, ConfigError, Spi};
use esp_hal::time::{Duration, Instant, Rate};

pub const WIDTH: usize = 152;
pub const HEIGHT: usize = 296;
pub const BUFFER_SIZE: usize = (WIDTH * HEIGHT) / 8;

const BUSY_TIMEOUT_MS: u64 = 5000;
const BUSY_POLL_MS: u32 = 10;
const RESET_DELAY_MS: u32 = 10;
const SLEEP_DELAY_MS: u32 = 1500;
const BORDER_WHITE: u8 = 0x97;

#[derive(Debug)]
pub enum Error {
    // Payloads are only ever inspected via the derived Debug impl, which
    // rustc's dead-code analysis intentionally ignores.
    #[allow(dead_code)]
    Spi(esp_hal::spi::Error),
    SpiConfig(ConfigError),
    BusyTimeout,
}

impl From<esp_hal::spi::Error> for Error {
    fn from(e: esp_hal::spi::Error) -> Self {
        Error::Spi(e)
    }
}

impl From<ConfigError> for Error {
    fn from(e: ConfigError) -> Self {
        Error::SpiConfig(e)
    }
}

/// Full-refresh (GC) LUT, byte-identical to the known-good C driver.
#[allow(clippy::large_stack_frames)]
static LUT_GC: [u8; 376] = [
    // VCOM
    0x05, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, //
    0x01, 0x00, 0x0E, 0x0E, 0x01, 0x0E, 0x0E, 0x0E, //
    0x01, 0x00, 0x18, 0x18, 0x01, 0x0E, 0x0E, 0x01, //
    0x01, 0x00, 0x23, 0x23, 0x01, 0x01, 0x01, 0x01, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // White to white
    0x05, 0x90, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, //
    0x01, 0x40, 0x0E, 0x0E, 0x01, 0x0E, 0x0E, 0x0E, //
    0x01, 0x90, 0x18, 0x18, 0x01, 0x0E, 0x0E, 0x01, //
    0x01, 0x80, 0x23, 0x23, 0x01, 0x01, 0x01, 0x01, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Black to white
    0x05, 0x90, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, //
    0x01, 0x40, 0x0E, 0x0E, 0x01, 0x0E, 0x0E, 0x0E, //
    0x01, 0x90, 0x18, 0x18, 0x01, 0x0E, 0x0E, 0x01, //
    0x01, 0x80, 0x23, 0x23, 0x01, 0x01, 0x01, 0x01, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // White to black
    0x05, 0x90, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, //
    0x01, 0x20, 0x0E, 0x0E, 0x01, 0x0E, 0x0E, 0x0E, //
    0x01, 0x18, 0x18, 0x18, 0x01, 0x0E, 0x0E, 0x01, //
    0x01, 0x10, 0x23, 0x23, 0x01, 0x01, 0x01, 0x01, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Black to black
    0x05, 0x90, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, //
    0x01, 0x20, 0x0E, 0x0E, 0x01, 0x0E, 0x0E, 0x0E, //
    0x01, 0x18, 0x18, 0x18, 0x01, 0x0E, 0x0E, 0x01, //
    0x01, 0x10, 0x23, 0x23, 0x01, 0x01, 0x01, 0x01, //
    // Trailing zero padding: the C driver declares the array as [u8; 376] but
    // initializes only 328 bytes; the remaining 48 bytes are zero (all 5 LUT
    // segments uploaded: 80 + 56 + 80 + 80 + 80 = 376).
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

pub struct Uc8251d<'d> {
    spi: Spi<'d, Blocking>,
    cs: Output<'d>,
    dc: Output<'d>,
    reset: Output<'d>,
    busy: Input<'d>,
    vin: Output<'d>,
    delay: Delay,
}

impl<'d> Uc8251d<'d> {
    /// Initialize the panel with the fixed Quote/0 pinout:
    /// SCK=10, MOSI=7, CS=6, DC=5, RESET=4, BUSY=3, VIN(power enable)=20.
    #[allow(clippy::too_many_arguments)] // each arg is a distinct MCU pin
    pub fn new(
        spi2: SPI2<'d>,
        sck: GPIO10<'d>,
        mosi: GPIO7<'d>,
        cs: GPIO6<'d>,
        dc: GPIO5<'d>,
        reset: GPIO4<'d>,
        busy: GPIO3<'d>,
        vin: GPIO20<'d>,
    ) -> Result<Self, Error> {
        let spi_config = SpiConfig::default()
            .with_frequency(Rate::from_mhz(15))
            .with_mode(Mode::_0);
        // CS is driven manually: the blocking driver chunks long writes into
        // 64-byte FIFO transfers, and hardware CS would toggle between chunks.
        let spi = Spi::new(spi2, spi_config)?.with_sck(sck).with_mosi(mosi);

        let mut this = Self {
            spi,
            cs: Output::new(cs, Level::High, OutputConfig::default()),
            dc: Output::new(dc, Level::High, OutputConfig::default()),
            reset: Output::new(reset, Level::High, OutputConfig::default()),
            busy: Input::new(busy, InputConfig::default().with_pull(Pull::Up)),
            vin: Output::new(vin, Level::High, OutputConfig::default()),
            delay: Delay::new(),
        };

        this.delay.delay_millis(10);
        this.reset_panel();
        this.wait_busy()?;
        this.power_on_panel()?;
        Ok(this)
    }

    /// Display one full-screen 1bpp frame (`BUFFER_SIZE` bytes, 1 = white).
    pub fn display(&mut self, frame: &[u8]) -> Result<(), Error> {
        assert_eq!(
            frame.len(),
            BUFFER_SIZE,
            "frame must be {BUFFER_SIZE} bytes"
        );
        self.vin.set_high();
        self.reset_panel();
        self.wait_busy()?;
        self.power_on_panel()?;
        self.write_frame(0xFF, frame)?;
        self.update_panel()
    }

    /// Clear the panel to white (or black).
    pub fn clear(&mut self, white: bool) -> Result<(), Error> {
        self.vin.set_high();
        self.reset_panel();
        self.wait_busy()?;
        self.power_on_panel()?;
        self.load_lut()?;
        self.cmd(0x10)?;
        self.write_filled_plane(0xFF)?;
        self.cmd(0x13)?;
        self.write_filled_plane(if white { 0xFF } else { 0x00 })?;
        self.update_panel()
    }

    /// Deep sleep the panel and cut its power enable pin.
    #[allow(dead_code)]
    pub fn sleep(&mut self) -> Result<(), Error> {
        self.wait_busy()?;
        self.delay.delay_millis(SLEEP_DELAY_MS);
        self.cmd(0x07)?;
        self.data(0xA5)?;
        self.vin.set_low();
        Ok(())
    }

    /// One SPI transaction: CS low, write, CS high.
    fn write_bus(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.cs.set_low();
        let result = self.spi.write(bytes);
        self.cs.set_high();
        result.map_err(Error::Spi)
    }

    fn cmd(&mut self, value: u8) -> Result<(), Error> {
        self.dc.set_low();
        self.write_bus(&[value])
    }

    fn data(&mut self, value: u8) -> Result<(), Error> {
        self.dc.set_high();
        self.write_bus(&[value])
    }

    fn data_bulk(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.dc.set_high();
        self.write_bus(bytes)
    }

    fn reset_panel(&mut self) {
        self.reset.set_high();
        self.delay.delay_millis(RESET_DELAY_MS);
        self.reset.set_low();
        self.delay.delay_millis(RESET_DELAY_MS);
        self.reset.set_high();
        self.delay.delay_millis(RESET_DELAY_MS);
    }

    fn wait_busy(&mut self) -> Result<(), Error> {
        let start = Instant::now();
        while self.busy.is_low() {
            self.cmd(0x71)?;
            self.delay.delay_millis(BUSY_POLL_MS);
            if start.elapsed() > Duration::from_millis(BUSY_TIMEOUT_MS) {
                return Err(Error::BusyTimeout);
            }
        }
        Ok(())
    }

    fn load_lut(&mut self) -> Result<(), Error> {
        self.cmd(0x20)?;
        self.data_bulk(&LUT_GC[0..80])?;
        self.cmd(0x21)?;
        self.data_bulk(&LUT_GC[80..136])?;
        self.cmd(0x22)?;
        self.data_bulk(&LUT_GC[136..216])?;
        self.cmd(0x23)?;
        self.data_bulk(&LUT_GC[216..296])?;
        self.cmd(0x24)?;
        self.data_bulk(&LUT_GC[296..376])
    }

    fn power_on_panel(&mut self) -> Result<(), Error> {
        self.cmd(0x00)?;
        self.data(0xF3)?;
        self.data(0x0E)?;

        self.cmd(0x01)?;
        self.data(0x03)?;
        self.data(0x00)?;
        self.data(0x3F)?;
        self.data(0x3F)?;
        self.data(0x03)?;

        self.cmd(0x06)?;
        self.data(0x17)?;
        self.data(0x17)?;
        self.data(0x17)?;

        self.cmd(0x61)?;
        self.data(WIDTH as u8)?;
        self.data((HEIGHT / 256) as u8)?;
        self.data((HEIGHT % 256) as u8)?;

        self.cmd(0x30)?;
        self.data(0x1B)?;

        self.cmd(0x60)?;
        self.data(0x22)?;

        self.cmd(0x82)?;
        self.data(0x00)?;

        self.cmd(0x03)?;
        self.data(0x10)?;

        self.cmd(0x50)?;
        self.data(BORDER_WHITE)?;

        self.cmd(0x04)?;
        self.delay.delay_millis(100);
        self.wait_busy()
    }

    fn update_panel(&mut self) -> Result<(), Error> {
        self.cmd(0x12)?;
        self.delay.delay_millis(100);
        self.wait_busy()
    }

    fn write_filled_plane(&mut self, value: u8) -> Result<(), Error> {
        let fill = [value; 128];
        let mut written = 0;
        while written < BUFFER_SIZE {
            let len = (BUFFER_SIZE - written).min(fill.len());
            self.data_bulk(&fill[..len])?;
            written += len;
        }
        Ok(())
    }

    fn write_frame(&mut self, old_value: u8, new_frame: &[u8]) -> Result<(), Error> {
        self.load_lut()?;
        self.cmd(0x10)?;
        self.write_filled_plane(old_value)?;
        self.cmd(0x13)?;
        self.data_bulk(new_frame)
    }
}
