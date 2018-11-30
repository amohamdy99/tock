//! Provides userspace with access to a serial interface.
//!
//! Setup
//! -----
//!
//! You need a device that provides the `hil::uart::UART` trait.
//!
//! ```rust
//! let console = static_init!(
//!     Console<usart::USART>,
//!     Console::new(&usart::USART0,
//!                  115200,
//!                  &mut console::WRITE_BUF,
//!                  &mut console::READ_BUF,
//!                  kernel::Grant::create()));
//! hil::uart::UART::set_client(&usart::USART0, console);
//! ```
//!
//! Usage
//! -----
//!
//! The user must perform three steps in order to write a buffer:
//!
//! ```c
//! // (Optional) Set a callback to be invoked when the buffer has been written
//! subscribe(CONSOLE_DRIVER_NUM, 1, my_callback);
//! // Share the buffer from userspace with the driver
//! allow(CONSOLE_DRIVER_NUM, buffer, buffer_len_in_bytes);
//! // Initiate the transaction
//! command(CONSOLE_DRIVER_NUM, 1, len_to_write_in_bytes)
//! ```
//!
//! When the buffer has been written successfully, the buffer is released from
//! the driver. Successive writes must call `allow` each time a buffer is to be
//! written.

use core::cmp;
use kernel::common::cells::{OptionalCell, TakeCell};
use kernel::hil::uart;
use kernel::{AppId, AppSlice, Callback, Error, Driver, Grant, ReturnCode, Success, Shared};

/// Syscall driver number.
// use driver;
pub const DRIVER_NUM: usize = 0;

#[derive(Default)]
pub struct App {
    write_callback: Option<Callback>,
    write_buffer: Option<AppSlice<Shared, u8>>,
    write_len: usize,
    write_remaining: usize, // How many bytes didn't fit in the buffer and still need to be printed.
    pending_write: bool,

    read_callback: Option<Callback>,
    read_buffer: Option<AppSlice<Shared, u8>>,
    read_len: usize,
}

pub static mut WRITE_BUF: [u8; 64] = [0; 64];
pub static mut READ_BUF: [u8; 64] = [0; 64];

pub struct Console<'a> {
    uart: &'a uart::UartData<'a>,
    apps: Grant<App>,
    tx_in_progress: OptionalCell<AppId>,
    tx_buffer: TakeCell<'static, [u8]>,
    rx_in_progress: OptionalCell<AppId>,
    rx_buffer: TakeCell<'static, [u8]>,
    baud_rate: u32,
}

impl Console<'a> {
    pub fn new(
        uart: &'a uart::UartData<'a>,
        baud_rate: u32,
        tx_buffer: &'static mut [u8],
        rx_buffer: &'static mut [u8],
        grant: Grant<App>,
    ) -> Console<'a> {
        Console {
            uart: uart,
            apps: grant,
            tx_in_progress: OptionalCell::empty(),
            tx_buffer: TakeCell::new(tx_buffer),
            rx_in_progress: OptionalCell::empty(),
            rx_buffer: TakeCell::new(rx_buffer),
            baud_rate: baud_rate,
        }
    }

    /// Internal helper function for setting up a new send transaction
    fn send_new(&self, app_id: AppId, app: &mut App, len: usize) -> ReturnCode {
        if let Some(slice) = app.write_buffer.take() {
            app.write_len = cmp::min(len, slice.len());
            app.write_remaining = app.write_len;
            self.send(app_id, app, slice);
            Ok(Success::Success)
        } else {
            Err(Error::EBUSY)
        }
    }

    /// Internal helper function for continuing a previously set up transaction
    /// Returns true if this send is still active, or false if it has completed
    fn send_continue(&self, app_id: AppId, app: &mut App) -> Result<bool, ReturnCode> {
        if app.write_remaining > 0 {
            app.write_buffer
                .take()
                .map_or(Err(Err(Error::ERESERVE)), |slice| {
                    self.send(app_id, app, slice);
                    Ok(true)
                })
        } else {
            Ok(false)
        }
    }

    /// Internal helper function for sending data for an existing transaction.
    /// Cannot fail. If can't send now, it will schedule for sending later.
    fn send(&self, app_id: AppId, app: &mut App, slice: AppSlice<Shared, u8>) {
        if self.tx_in_progress.is_none() {
            self.tx_in_progress.set(app_id);
            self.tx_buffer.take().map(|buffer| {
                let mut transaction_len = app.write_remaining;
                for (i, c) in slice.as_ref()[slice.len() - app.write_remaining..slice.len()]
                    .iter()
                    .enumerate()
                {
                    if buffer.len() <= i {
                        break;
                    }
                    buffer[i] = *c;
                }

                // Check if everything we wanted to print
                // fit in the buffer.
                if app.write_remaining > buffer.len() {
                    transaction_len = buffer.len();
                    app.write_remaining -= buffer.len();
                    app.write_buffer = Some(slice);
                } else {
                    app.write_remaining = 0;
                }

                // FIXME: what to do here?
                self.uart.transmit_buffer(buffer, transaction_len)
                    .map(|_success | ())
                    .map_err(|_err| ());
            });
        } else {
            app.pending_write = true;
            app.write_buffer = Some(slice);
        }
    }

    /// Internal helper function for starting a receive operation
    fn receive_new(&self, app_id: AppId, app: &mut App, len: usize) -> ReturnCode {
        let rx_buf = match self.rx_buffer.take() {
            Some(buffer) => buffer,
            None => return Err(Error::EBUSY),
        };

        match app.read_buffer {
            Some(ref slice) => {
                let read_len = cmp::min(len, slice.len());
                if read_len > self.rx_buffer.map_or(0, |buf| buf.len()) {
                    // For simplicity, impose a small maximum receive length
                    // instead of doing incremental reads
                    Err(Error::ESIZE)
                } else {
                    // Note: We have ensured above that rx_buffer is present
                    app.read_len = read_len;
                    self.rx_in_progress.set(app_id);
                    self.uart.receive_buffer(rx_buf, app.read_len).map_err(|err| {
                        // static mut [u8] buffer is borrowed here, thus can't move it
                        self.rx_buffer.replace(err.buffer);
                        err.error.into()
                    })
                }
            }
            None => Err(Error::EINVAL),
        }
    }
}

impl Driver for Console<'a> {
    /// Setup shared buffers.
    ///
    /// ### `allow_num`
    ///
    /// - `1`: Writeable buffer for write buffer
    /// - `2`: Writeable buffer for read buffer
    fn allow(
        &self,
        appid: AppId,
        allow_num: usize,
        slice: Option<AppSlice<Shared, u8>>,
    ) -> ReturnCode {
        match allow_num {
            1 => self
                .apps
                .enter(appid, |app, _| {
                    app.write_buffer = slice;
                    Success::Success
                }).map_err(Into::into),
            2 => self
                .apps
                .enter(appid, |app, _| {
                    app.read_buffer = slice;
                    Success::Success
                }).map_err(Into::into),
            _ => Err(Error::ENOSUPPORT),
        }
    }

    /// Setup callbacks.
    ///
    /// ### `subscribe_num`
    ///
    /// - `1`: Write buffer completed callback
    fn subscribe(
        &self,
        subscribe_num: usize,
        callback: Option<Callback>,
        app_id: AppId,
    ) -> ReturnCode {
        match subscribe_num {
            1 /* putstr/write_done */ => {
                self.apps.enter(app_id, |app, _| {
                    app.write_callback = callback;
                    Success::Success
                }).map_err(Into::into)
            },
            2 /* getnstr done */ => {
                self.apps.enter(app_id, |app, _| {
                    app.read_callback = callback;
                    Success::Success
                }).map_err(Into::into)
            },
            _ => Err(Error::ENOSUPPORT)
        }
    }

    /// Initiate serial transfers
    ///
    /// ### `command_num`
    ///
    /// - `0`: Driver check.
    /// - `1`: Transmits a buffer passed via `allow`, up to the length
    ///        passed in `arg1`
    /// - `2`: Receives into a buffer passed via `allow`, up to the length
    ///        passed in `arg1`
    /// - `3`: Cancel any in progress receives and return (via callback)
    ///        what has been received so far.
    fn command(&self, cmd_num: usize, arg1: usize, _: usize, appid: AppId) -> ReturnCode {
        match cmd_num {
            0 /* check if present */ => Ok(Success::Success),
            1 /* putstr */ => {
                let len = arg1;
                self.apps.enter(appid, |app, _| {
                    self.send_new(appid, app, len)
                }).unwrap_or_else(Into::into)
            },
            2 /* getnstr */ => {
                let len = arg1;
                self.apps.enter(appid, |app, _| {
                    self.receive_new(appid, app, len)
                }).unwrap_or_else(|err| err.into())
            },
            3 /* abort rx */ => {
                self.uart.receive_abort();
                Ok(Success::Success)
            }
            _ => Err(Error::ENOSUPPORT)
        }
    }
}

impl uart::TransmitClient for Console<'a> {
    fn transmitted_buffer(&self, buffer: &'static mut [u8], _tx_len: usize, _rcode: ReturnCode) {
        // Either print more from the AppSlice or send a callback to the
        // application.
        self.tx_buffer.replace(buffer);
        self.tx_in_progress.take().map(|appid| {
            self.apps.enter(appid, |app, _| {
                match self.send_continue(appid, app) {
                    Ok(more_to_send) => {
                        if !more_to_send {
                            // Go ahead and signal the application
                            let written = app.write_len;
                            app.write_len = 0;
                            app.write_callback.map(|mut cb| {
                                cb.schedule(written, 0, 0);
                            });
                        }
                    }
                    Err(return_code) => {
                        // XXX This shouldn't ever happen?
                        app.write_len = 0;
                        app.write_remaining = 0;
                        app.pending_write = false;
                        let r0: usize = match return_code {
                            Ok(s) => s.into(),
                            Err(e) => e.into(),
                        };
                        app.write_callback.map(|mut cb| {
                            cb.schedule(r0, 0, 0);
                        });
                    }
                }
            })
        });

        // If we are not printing more from the current AppSlice,
        // see if any other applications have pending messages.
        if self.tx_in_progress.is_none() {
            for cntr in self.apps.iter() {
                let started_tx = cntr.enter(|app, _| {
                    if app.pending_write {
                        app.pending_write = false;
                        match self.send_continue(app.appid(), app) {
                            Ok(more_to_send) => more_to_send,
                            Err(return_code) => {
                                // XXX This shouldn't ever happen?
                                app.write_len = 0;
                                app.write_remaining = 0;
                                app.pending_write = false;
                                let r0: usize = match return_code {
                                    Ok(s) => s.into(),
                                    Err(e) => e.into(),
                                };
                                app.write_callback.map(|mut cb| {
                                    cb.schedule(r0, 0, 0);
                                });
                                false
                            }
                        }
                    } else {
                        false
                    }
                });
                if started_tx {
                    break;
                }
            }
        }
    }
}

impl uart::ReceiveClient for Console<'a> {
    fn received_buffer(&self, buffer: &'static mut [u8], rx_len: usize, error: uart::UartError) {
        self.rx_in_progress
            .take()
            .map(|appid| {
                self.apps
                    .enter(appid, |app, _| {
                        app.read_callback.map(|mut cb| {
                            // An iterator over the returned buffer yielding only the first `rx_len`
                            // bytes
                            let rx_buffer = buffer.iter().take(rx_len);
                            match error.error {
                                uart::Error::None | uart::Error::Aborted => {
                                    // Receive some bytes, signal error type and return bytes to process buffer
                                    if let Some(mut app_buffer) = app.read_buffer.take() {
                                        for (a, b) in app_buffer.iter_mut().zip(rx_buffer) {
                                            *a = *b;
                                        }
                                        let rettype: usize = if error.error == uart::Error::None {
                                            Success::Success.into()
                                        } else {
                                            Error::ECANCEL.into()
                                        };
                                        cb.schedule(rettype, rx_len, 0);
                                    } else {
                                        // Oops, no app buffer
                                        cb.schedule(From::from(Error::EINVAL), 0, 0);
                                    }
                                }
                                _ => {
                                    // Some UART error occurred
                                    cb.schedule(From::from(Error::FAIL), 0, 0);
                                }
                            }
                        });
                    }).unwrap_or_default();
            }).unwrap_or_default();

        // Whatever happens, we want to make sure to replace the rx_buffer for future transactions
        self.rx_buffer.replace(buffer);
    }
}
