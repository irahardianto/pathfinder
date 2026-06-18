use super::*;
use std::io::Error;

#[test]
fn test_from_io_error() {
    let io_err = Error::new(std::io::ErrorKind::NotFound, "test");
    let surgeon_err = SurgeonError::from(io_err);
    assert!(matches!(surgeon_err, SurgeonError::Io(_)));

    let pathfinder_err: pathfinder_common::error::PathfinderError = surgeon_err.into();
    assert!(matches!(
        pathfinder_err,
        pathfinder_common::error::PathfinderError::IoError { .. }
    ));
}
