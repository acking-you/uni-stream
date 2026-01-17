#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(all(doc), feature(doc_auto_cfg))]

pub mod addr;
pub mod stream;
pub mod udp;
