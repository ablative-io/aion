//! handler invocation, payload decode/encode, failure classification

pub use crate::activity::{
    ActivityRegistry as TypedActivityDispatcher, decode_payload, encode_payload,
};
