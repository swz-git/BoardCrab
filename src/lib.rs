pub mod board;
pub mod fen;
pub mod search;
pub mod move_gen;
pub mod eval;
pub mod transpos;

mod bitmask;
mod lookup_gen;
mod zobrist;

static INIT_ONCE: std::sync::Once = std::sync::Once::new();

fn _init() {
    lookup_gen::init();
    zobrist::init();
}

pub fn init() {
    INIT_ONCE.call_once(_init);
}