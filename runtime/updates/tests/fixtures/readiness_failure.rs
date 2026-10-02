// A native process fixture which exits before the required framed ready handshake.
// This is intentionally not an activation owner or a successful worker implementation.
fn main() { std::process::exit(7); }
