use super::*;

#[test]
fn work_reads_against_its_limit() {
    let work = WorkLimit {
        limit: 1000,
        sampled: None,
    };
    let memory = work.against(400);
    assert_eq!((memory.total, memory.available), (1000, 600));
    assert!(memory.used_at_least(40) && !memory.used_at_least(41));
    // Work past its limit leaves nothing.
    assert_eq!(work.against(1200).available, 0);
}

#[test]
fn work_outside_the_slice_counts_beside_it() {
    let work = WorkLimit {
        limit: 1000,
        sampled: Some(SampledWork {
            all: 700,
            outside_slice: 300,
        }),
    };
    // One command started in the slice, another directly beside it.
    let slice = Slice {
        used: 600,
        stall: None,
    };
    assert_eq!(work.used(Some(slice)).unwrap().0, 900);
    // Before the slice exists, the sample counts all of it.
    assert_eq!(work.used(None).unwrap().0, 700);
    // Without a current sample, no reading admits work, slice or not.
    let unsampled = WorkLimit {
        sampled: None,
        ..work
    };
    assert!(unsampled.used(Some(slice)).is_err());
    assert!(unsampled.used(None).is_err());
}
