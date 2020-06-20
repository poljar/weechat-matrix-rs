use std::io;
use weechat::Weechat;

#[derive(Clone)]
pub struct Debug();

impl Debug {
    pub async fn write_helper(message: Vec<u8>) {
        let message = String::from_utf8(message).unwrap();
        let message =
            Weechat::execute_modifier("color_decode_ansi", "1", &message)
                .unwrap();
        Weechat::print(&message)
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
