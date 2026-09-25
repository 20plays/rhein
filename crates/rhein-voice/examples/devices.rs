//! Lists the audio devices calls can use (nothing is opened).

fn main() {
    println!("inputs:");
    for device in rhein_voice::audio::input_devices() {
        println!("  {}  ({})", device.name, device.id);
    }
    println!("outputs:");
    for device in rhein_voice::audio::output_devices() {
        println!("  {}  ({})", device.name, device.id);
    }
}
