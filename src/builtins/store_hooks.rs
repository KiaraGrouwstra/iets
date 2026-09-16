//! Seam to the store module (branch `store`). Until it is linked, anything
//! that needs a store path computation is a hard error.

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};

pub fn copy_path_to_store(
    _ev: &mut Evaluator,
    path: &'static [u8],
    pos: Pos,
) -> EvalResult<&'static [u8]> {
    // TODO(store): NAR-hash `path` and return makeFixedOutputPath(recursive, sha256, basename).
    Err(Error::boxed(
        ErrorKind::Eval,
        format!(
            "copying '{}' to the store is not implemented yet",
            String::from_utf8_lossy(path)
        ),
        pos,
    ))
}
