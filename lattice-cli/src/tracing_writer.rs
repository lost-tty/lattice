//! Custom tracing writer that routes logs through rustyline-async's SharedWriter

use rustyline_async::SharedWriter;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

/// A writer that writes directly to SharedWriter
pub struct SharedWriterWrapper {
    writer: Arc<Mutex<SharedWriter>>,
}

impl SharedWriterWrapper {
    pub fn new(writer: Arc<Mutex<SharedWriter>>) -> Self {
        Self { writer }
    }
}

impl Write for SharedWriterWrapper {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut writer) = self.writer.lock() {
            writer.write(buf)
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Ok(mut writer) = self.writer.lock() {
            writer.flush()
        } else {
            Ok(())
        }
    }
}

/// A MakeWriter implementation that creates SharedWriterWrappers
#[derive(Clone)]
pub struct SharedWriterMakeWriter {
    writer: Arc<Mutex<SharedWriter>>,
}

impl SharedWriterMakeWriter {
    /// Create a new SharedWriterMakeWriter
    pub fn new(shared_writer: SharedWriter) -> Self {
        Self {
            writer: Arc::new(Mutex::new(shared_writer)),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedWriterMakeWriter {
    type Writer = SharedWriterWrapper;

    fn make_writer(&'a self) -> Self::Writer {
        SharedWriterWrapper::new(self.writer.clone())
    }
}
