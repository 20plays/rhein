//! Lists the audio devices calls can use (nothing is opened), and whether the desktop can share
//! a screen.

fn main() {
    println!("inputs:");
    for device in rhein_voice::audio::input_devices() {
        println!("  {}  ({})", device.name, device.id);
    }
    println!("outputs:");
    for device in rhein_voice::audio::output_devices() {
        println!("  {}  ({})", device.name, device.id);
    }
    match rhein_voice::screen::portal_ready() {
        Ok(()) => println!("screen capture portal: ready"),
        Err(err) => println!("screen capture portal: {err}"),
    }
}
