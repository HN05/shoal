use super::*;

#[tokio::test]
async fn the_line_takes_each_space_once_in_the_order_it_joined() {
    let line = Line::default();
    let (first, second) = (SpaceId::from("first"), SpaceId::from("second"));
    for id in [&first, &second, &first] {
        line.join(id);
    }
    assert_eq!(line.next().await, first);
    assert_eq!(line.next().await, second);
    // Once taken, a space joins again at the end.
    line.join(&first);
    assert_eq!(line.next().await, first);
    assert!(line.queued.lock().unwrap().is_empty());
}
