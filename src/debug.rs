use crate::Matrix;
use std::cell::RefMut;
use std::io;
use weechat::{
    buffer::{BufferHandle, BufferSettings},
    Weechat,
};

#[derive(Clone)]
pub struct Debug();

impl Debug {
    fn create_debug_buffer(debug_buffer: &mut RefMut<Option<BufferHandle>>) {
        let buffer = Weechat::buffer_new(BufferSettings::new("Matrix debug"))
            .expect("Can't create Matrix debug buffer");
        **debug_buffer = Some(buffer);
    }

    async fn write_helper(message: Vec<u8>) {
        let matrix = Matrix::get();

        let message = String::from_utf8(message).unwrap();
        let message =
            Weechat::execute_modifier("color_decode_ansi", "1", &message)
                .unwrap();

        let mut debug_buffer = matrix.debug_buffer.borrow_mut();

        if matrix.config.borrow().network().debug_buffer() {
            let buffer = if let Some(buffer) = debug_buffer.as_ref() {
                if let Ok(buffer) = buffer.upgrade() {
                    buffer
                } else {
                    Debug::create_debug_buffer(&mut debug_buffer);
                    debug_buffer.as_ref().unwrap().upgrade().unwrap()
                }
            } else {
                Debug::create_debug_buffer(&mut debug_buffer);
                debug_buffer.as_ref().unwrap().upgrade().unwrap()
            };

            buffer.print(&message);
        } else {
            Weechat::print(&message)
        }
    }
}

impl io::Write for Debug {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Weechat::spawn_from_thread(Debug::write_helper(buf.to_owned()));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
