//! The fixed SQL of the exact interval arithmetic answers.
//!
//! The field parse is the comparison key's read of its operand text, kept
//! byte for byte so an arithmetic answer reads the fields the key reads. The
//! render is the text PostgreSQL prints for `(months, days, micros)` under
//! its default `IntervalStyle`, `EncodeInterval` with `INTSTYLE_POSTGRES`:
//! `NULL` when a field is missing, has left the 32-bit or 64-bit range
//! PostgreSQL's `interval` fields hold, or lands on either infinity bound.

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
