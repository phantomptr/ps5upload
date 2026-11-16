//! Game archives read in place, for Convert on a computer (the engine, through
//! `ps5upload-core::archive_stream`) and on the PS5 (`ps5upload-convert`): [`stream`] lists an
//! archive and serves its files in any order without unpacking it; [`names`] holds the entry
//! name checks the engine's unpack and transfer code use as well.

pub mod names;
pub mod stream;
