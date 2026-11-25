//! SQL that rewrites a recorded path inside a moving directory to the same
//! place inside its destination, bound as `?2` and `?3`.

/// SQL that holds when the path in `column` is the directory `?2` or lies
/// inside it.
pub fn path_inside(column: &str) -> String {
    format!("({column}=?2 OR substr({column},1,length(?2)+1)=?2||'/')")
}

/// SQL for the path in `column` with the directory `?2` it is or lies in
/// replaced by `?3`, and for any other path unchanged.
pub fn path_rebased(column: &str) -> String {
    format!(
        "CASE WHEN {} THEN ?3||substr({column},length(?2)+1) ELSE {column} END",
        path_inside(column)
    )
}
