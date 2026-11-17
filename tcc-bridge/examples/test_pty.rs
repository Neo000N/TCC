use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn main() {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    }).unwrap();

    let mut cmd = CommandBuilder::new("bash");
    cmd.args(&["-i"]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd).unwrap();
    
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    
    // Write command
    writer.write_all(b"echo hello\n").unwrap();
    writer.flush().unwrap();
    
    // Read output
    let mut buf = [0u8; 1024];
    match reader.read(&mut buf) {
        Ok(n) => {
            println!("Read {} bytes: {:?}", n, &buf[..n]);
            println!("As string: {}", String::from_utf8_lossy(&buf[..n]));
        }
        Err(e) => println!("Read error: {}", e),
    }
    
    child.wait().unwrap();
}
