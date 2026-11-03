//! The fixed SQL of the exact interval arithmetic answers.
//!
//! The field parse is the comparison key's read of its operand text, kept
//! byte for byte so an arithmetic answer reads the fields the key reads. The
//! render is the text PostgreSQL prints for `(months, days, micros)` under
//! its default `IntervalStyle`, `EncodeInterval` with `INTSTYLE_POSTGRES`:
//! `NULL` when a field is missing, has left the 32-bit or 64-bit range
//! PostgreSQL's `interval` fields hold, or lands on either infinity bound.

#[cfg(not(feature = "std"))]
use alloc::{
    format,
    string::{String, ToString},
};

/// The operand placeholder the single-operand templates substitute.
pub(super) const PLACEHOLDER: &str = crate::impls::interval::keys_templates::PLACEHOLDER;

/// The second operand placeholder the two-operand templates substitute.
pub(super) const PLACEHOLDER_2: &str = "pg2sqlite_interval_operand_2";

/// The key's field parse of its operand text, the inner select of
/// `BOUND_KEY` from its `v` projection to the operand placeholder.
pub(super) const FIELD_PARSE: &str = "(SELECT v, (CASE json_extract(a, '$[1]') WHEN 'year' THEN 12 * CAST(json_extract(a, '$[0]') AS INTEGER) WHEN 'years' THEN 12 * CAST(json_extract(a, '$[0]') AS INTEGER) WHEN 'mon' THEN 1 * CAST(json_extract(a, '$[0]') AS INTEGER) WHEN 'mons' THEN 1 * CAST(json_extract(a, '$[0]') AS INTEGER) ELSE 0 END + CASE json_extract(a, '$[3]') WHEN 'year' THEN 12 * CAST(json_extract(a, '$[2]') AS INTEGER) WHEN 'years' THEN 12 * CAST(json_extract(a, '$[2]') AS INTEGER) WHEN 'mon' THEN 1 * CAST(json_extract(a, '$[2]') AS INTEGER) WHEN 'mons' THEN 1 * CAST(json_extract(a, '$[2]') AS INTEGER) ELSE 0 END + CASE json_extract(a, '$[5]') WHEN 'year' THEN 12 * CAST(json_extract(a, '$[4]') AS INTEGER) WHEN 'years' THEN 12 * CAST(json_extract(a, '$[4]') AS INTEGER) WHEN 'mon' THEN 1 * CAST(json_extract(a, '$[4]') AS INTEGER) WHEN 'mons' THEN 1 * CAST(json_extract(a, '$[4]') AS INTEGER) ELSE 0 END) AS m, (CASE json_extract(a, '$[1]') WHEN 'day' THEN 1 * CAST(json_extract(a, '$[0]') AS INTEGER) WHEN 'days' THEN 1 * CAST(json_extract(a, '$[0]') AS INTEGER) ELSE 0 END + CASE json_extract(a, '$[3]') WHEN 'day' THEN 1 * CAST(json_extract(a, '$[2]') AS INTEGER) WHEN 'days' THEN 1 * CAST(json_extract(a, '$[2]') AS INTEGER) ELSE 0 END + CASE json_extract(a, '$[5]') WHEN 'day' THEN 1 * CAST(json_extract(a, '$[4]') AS INTEGER) WHEN 'days' THEN 1 * CAST(json_extract(a, '$[4]') AS INTEGER) ELSE 0 END) AS d, (CASE WHEN instr(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), ':') = 0 THEN 0 WHEN substr(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), 1, 1) = '-' THEN -(CAST(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), 1, instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') - 1) AS INTEGER) * 3600000000 + CAST(substr(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') + 1), 1, 2) AS INTEGER) * 60000000 + CAST(substr(substr(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') + 1), 4), 1, 2) AS INTEGER) * 1000000 + CASE WHEN length(substr(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') + 1), 4)) > 2 THEN CAST(substr(substr(substr(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') + 1), 4), 4) || '000000', 1, 6) AS INTEGER) ELSE 0 END) ELSE (CAST(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), 1, instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') - 1) AS INTEGER) * 3600000000 + CAST(substr(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') + 1), 1, 2) AS INTEGER) * 60000000 + CAST(substr(substr(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') + 1), 4), 1, 2) AS INTEGER) * 1000000 + CASE WHEN length(substr(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') + 1), 4)) > 2 THEN CAST(substr(substr(substr(substr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), instr(ltrim(json_extract(a, '$[' || (json_array_length(a) - 1) || ']'), '+-'), ':') + 1), 4), 4) || '000000', 1, 6) AS INTEGER) ELSE 0 END) END) AS u FROM (SELECT v, ('[\"' || replace(v, ' ', '\",\"') || '\"]') AS a FROM (SELECT pg2sqlite_interval_operand AS v)))";

/// The `m` field placeholder the render substitutes.
pub(super) const FIELD_M: &str = "__PG2SQLITE_M__";

/// The `d` field placeholder the render substitutes.
pub(super) const FIELD_D: &str = "__PG2SQLITE_D__";

/// The `u` field placeholder the render substitutes.
pub(super) const FIELD_U: &str = "__PG2SQLITE_U__";

/// The canonical text of the result fields `m`, `d` and `u`, `NULL` where
/// PostgreSQL raises or prints infinity.
pub(super) const RENDER: &str = "CASE
 WHEN __PG2SQLITE_M__ IS NULL OR __PG2SQLITE_D__ IS NULL OR __PG2SQLITE_U__ IS NULL THEN NULL
 WHEN typeof(__PG2SQLITE_U__) <> 'integer' THEN NULL
 WHEN __PG2SQLITE_M__ < -2147483648 OR __PG2SQLITE_M__ > 2147483647 OR __PG2SQLITE_D__ < -2147483648 OR __PG2SQLITE_D__ > 2147483647 THEN NULL
 WHEN __PG2SQLITE_M__ = 2147483647 AND __PG2SQLITE_D__ = 2147483647 AND __PG2SQLITE_U__ = 9223372036854775807 THEN NULL
 WHEN __PG2SQLITE_M__ = -2147483648 AND __PG2SQLITE_D__ = -2147483648 AND __PG2SQLITE_U__ = -9223372036854775808 THEN NULL
 ELSE
  (CASE WHEN __PG2SQLITE_M__ / 12 <> 0 THEN CAST(__PG2SQLITE_M__ / 12 AS TEXT) || ' year' || CASE WHEN __PG2SQLITE_M__ / 12 = 1 THEN '' ELSE 's' END ELSE '' END
   || CASE WHEN __PG2SQLITE_M__ % 12 <> 0 THEN (CASE WHEN __PG2SQLITE_M__ / 12 <> 0 THEN ' ' ELSE '' END) || CASE WHEN __PG2SQLITE_M__ / 12 < 0 AND __PG2SQLITE_M__ % 12 > 0 THEN '+' ELSE '' END || CAST(__PG2SQLITE_M__ % 12 AS TEXT) || ' mon' || CASE WHEN __PG2SQLITE_M__ % 12 = 1 THEN '' ELSE 's' END ELSE '' END
   || CASE WHEN __PG2SQLITE_D__ <> 0 THEN (CASE WHEN __PG2SQLITE_M__ <> 0 THEN ' ' ELSE '' END) || CASE WHEN (CASE WHEN __PG2SQLITE_M__ % 12 <> 0 THEN __PG2SQLITE_M__ % 12 < 0 ELSE __PG2SQLITE_M__ / 12 < 0 END) AND __PG2SQLITE_D__ > 0 THEN '+' ELSE '' END || CAST(__PG2SQLITE_D__ AS TEXT) || ' day' || CASE WHEN __PG2SQLITE_D__ = 1 THEN '' ELSE 's' END ELSE '' END
   || CASE WHEN __PG2SQLITE_U__ <> 0 OR (__PG2SQLITE_M__ = 0 AND __PG2SQLITE_D__ = 0) THEN (CASE WHEN __PG2SQLITE_M__ <> 0 OR __PG2SQLITE_D__ <> 0 THEN ' ' ELSE '' END) || CASE WHEN (CASE WHEN __PG2SQLITE_D__ <> 0 THEN __PG2SQLITE_D__ < 0 WHEN __PG2SQLITE_M__ % 12 <> 0 THEN __PG2SQLITE_M__ % 12 < 0 ELSE __PG2SQLITE_M__ / 12 < 0 END) AND __PG2SQLITE_U__ > 0 THEN '+' ELSE '' END
        || CASE WHEN __PG2SQLITE_U__ = -9223372036854775808 THEN '-2562047788:00:54.775808'
           ELSE (CASE WHEN __PG2SQLITE_U__ < 0 THEN '-' ELSE '' END
            || printf('%02d', ABS(__PG2SQLITE_U__) / 3600000000)
            || ':' || printf('%02d', (ABS(__PG2SQLITE_U__) / 1000000) % 3600 / 60)
            || ':' || printf('%02d', (ABS(__PG2SQLITE_U__) / 1000000) % 60)
            || CASE WHEN ABS(__PG2SQLITE_U__) % 1000000 = 0 THEN ''
                 ELSE '.' || CASE
                       WHEN ABS(__PG2SQLITE_U__) % 1000000 % 100000 = 0 THEN CAST(ABS(__PG2SQLITE_U__) % 1000000 / 100000 AS TEXT)
                       WHEN ABS(__PG2SQLITE_U__) % 1000000 % 10000 = 0 THEN printf('%02d', ABS(__PG2SQLITE_U__) % 1000000 / 10000)
                       WHEN ABS(__PG2SQLITE_U__) % 1000000 % 1000 = 0 THEN printf('%03d', ABS(__PG2SQLITE_U__) % 1000000 / 1000)
                       WHEN ABS(__PG2SQLITE_U__) % 1000000 % 100 = 0 THEN printf('%04d', ABS(__PG2SQLITE_U__) % 1000000 / 100)
                       WHEN ABS(__PG2SQLITE_U__) % 1000000 % 10 = 0 THEN printf('%05d', ABS(__PG2SQLITE_U__) % 1000000 / 10)
                       ELSE printf('%06d', ABS(__PG2SQLITE_U__) % 1000000)
                     END
                END)
           END
        ELSE '' END)
 END
END";

/// The factor placeholder a multiplication or division substitutes.
pub(super) const FACTOR: &str = "pg2sqlite_interval_factor";

/// PostgreSQL's `rint` on a `double`, ties to even: adding and taking away
/// `2^52` leaves no fraction bits on the magnitude, and a magnitude at or
/// above `2^52` is already integral.
fn rint(x: &str) -> String {
    format!(
        "(CASE WHEN ABS({x}) >= 4503599627370496.0 THEN {x} \
         WHEN {x} < 0.0 THEN -((-({x}) + 4503599627370496.0) - 4503599627370496.0) \
         ELSE ({x} + 4503599627370496.0) - 4503599627370496.0 END)"
    )
}

/// PostgreSQL's `TSROUND`, `rint(x * 1e6) / 1e6`.
fn tsround(x: &str) -> String {
    format!("({} / 1000000.0)", rint(&format!("{x} * 1000000.0")))
}

/// The canonical text of `interval_mul`, or `interval_div` when `divide`,
/// over a `source` row of an interval's `m`, `d` and `u` fields and the `f`
/// factor as a `REAL`.
///
/// Each derived table is one statement of PostgreSQL's function in its
/// order, so every intermediate double is the one PostgreSQL computes: the
/// month and day products truncated into their fields, the month fraction
/// cascaded to days and the day fraction to seconds under `TSROUND`, a whole
/// day of seconds carried into the days, and the time rounded with `rint`.
/// `NULL` wherever PostgreSQL raises `interval out of range` or `division by
/// zero`, and for fields already past the 32-bit or 64-bit range, which a
/// summed `avg` operand reaches where PostgreSQL's accumulation raises.
///
/// Every derived table carries `LIMIT -1 OFFSET 0`, which keeps SQLite from
/// flattening it into the next: a layer names its inputs more than once, so
/// flattening substitutes each input expression at every use, and the
/// expression grows exponentially with the depth.
pub(super) fn scaled(source: &str, divide: bool) -> String {
    let op = if divide { "/" } else { "*" };
    let zero_factor = if divide { "WHEN f = 0.0 THEN NULL " } else { "" };
    let render =
        RENDER.replace(FIELD_U, "CAST(t AS INTEGER)").replace(FIELD_D, "d2").replace(FIELD_M, "mo");
    let layers = [
        format!("m, d, u, f, m {op} f AS mp, d {op} f AS dp"),
        "*, CAST(mp AS INTEGER) AS mo, CAST(dp AS INTEGER) AS d0, \
         (mp - CAST(mp AS INTEGER)) * 30.0 AS mraw"
            .to_string(),
        format!("*, {} AS mrd", tsround("mraw")),
        "*, (dp - d0 + mrd - CAST(mrd AS INTEGER)) * 86400.0 AS sraw".to_string(),
        format!("*, {} AS s0", tsround("sraw")),
        "*, CASE WHEN ABS(s0) >= 86400.0 THEN CAST(s0 / 86400.0 AS INTEGER) ELSE 0 END AS carry"
            .to_string(),
        "*, d0 + carry AS d1, s0 - carry * 86400.0 AS sr".to_string(),
        format!("*, d1 + CAST(mrd AS INTEGER) AS d2, u {op} f + sr * 1000000.0 AS traw"),
        format!("*, {} AS t", rint("traw")),
    ];
    let rows = layers.iter().fold(source.to_string(), |from, columns| {
        format!("(SELECT {columns} FROM {from} LIMIT -1 OFFSET 0)")
    });
    format!(
        "(SELECT CASE \
         WHEN typeof(u) <> 'integer' OR m < -2147483648 OR m > 2147483647 \
         OR d < -2147483648 OR d > 2147483647 THEN NULL \
         {zero_factor}\
         WHEN NOT (mp >= -2147483648.0 AND mp < 2147483648.0) THEN NULL \
         WHEN NOT (dp >= -2147483648.0 AND dp < 2147483648.0) THEN NULL \
         WHEN d1 < -2147483648 OR d1 > 2147483647 THEN NULL \
         WHEN d2 < -2147483648 OR d2 > 2147483647 THEN NULL \
         WHEN NOT (t >= -9223372036854775808.0 AND t < 9223372036854775808.0) THEN NULL \
         ELSE {render} END FROM {rows})"
    )
}
