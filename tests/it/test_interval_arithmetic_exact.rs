//! The exact interval arithmetic answers, checked against PostgreSQL 18.6:
//! negation, addition and subtraction of intervals, multiplication and
//! division by a factor, their sum and average, and a date, time or
//! timestamp shifted by an interval.
//!
//! The differential pairs are canonical interval texts PostgreSQL stores, and
//! its measured answers to negating the first and adding or subtracting the
//! second, `NULL` where PostgreSQL raises.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::{
    run_translated_bound_with, run_translated_default, run_translated_with,
};

use crate::helpers::{run_translated as run_translated_helper, translate::assert_refused};

/// A differential pair: the two canonical interval texts PostgreSQL stores,
/// and its answers to negating the first and to adding or subtracting the
/// second, `None` where PostgreSQL raises.
type Pair<'a> = (&'a str, &'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);

/// The corpus of differential pairs, in `id` order.
const PAIRS: &[Pair<'static>] = &[
    (
        "1 year -1 days -01:00:00",
        "-14283278 years -6 mons +539360863 days",
        Some("-1 years +1 day 01:00:00"),
        Some("-14283277 years -6 mons +539360862 days -01:00:00"),
        Some("14283279 years 6 mons -539360864 days -01:00:00"),
    ),
    (
        "40967461 years 5 mons -198916822 days -51639315:05:16.989153",
        "-1 years +2147483647 days -2562047788:00:54.775806",
        Some("-40967461 years -5 mons +198916822 days 51639315:05:16.989153"),
        None,
        None,
    ),
    (
        "1 year 11 mons -2 days +00:00:00.99",
        "2 years -2147483648 days +00:00:00.99",
        Some("-1 years -11 mons +2 days -00:00:00.99"),
        None,
        Some("-1 mons +2147483646 days"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "178956970 years 7 mons -00:00:00.000001",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("178956969 years 6 mons 2562047788:00:54.775806"),
        None,
    ),
    (
        "1 mon 2 days 00:00:00.99",
        "-11 mons +2 days 01:00:00",
        Some("-1 mons -2 days -00:00:00.99"),
        Some("-10 mons +4 days 01:00:00.99"),
        Some("1 year -00:59:59.01"),
    ),
    (
        "2 years 11 mons 2 days -00:00:00.000001",
        "51764879 years 10 mons 543425336 days 19259456:58:56.752156",
        Some("-2 years -11 mons -2 days +00:00:00.000001"),
        Some("51764882 years 9 mons 543425338 days 19259456:58:56.752155"),
        Some("-51764876 years -11 mons -543425334 days -19259456:58:56.752157"),
    ),
    (
        "-1 mons -2 days -2562047788:00:54.775806",
        "-2 years -11 mons -2147483648 days -01:00:00",
        Some("1 mon 2 days 2562047788:00:54.775806"),
        None,
        Some("2 years 10 mons 2147483646 days -2562047787:00:54.775806"),
    ),
    (
        "-83858867 years -9 mons -462097977 days -52830097:47:29.797211",
        "-178956970 years -8 mons +01:00:00",
        Some("83858867 years 9 mons 462097977 days 52830097:47:29.797211"),
        None,
        Some("95098102 years 11 mons -462097977 days -52830098:47:29.797211"),
    ),
    (
        "113339416 years 748638078 days 00:00:00.000001",
        "178956970 years 6 mons -2 days +00:00:00.000001",
        Some("-113339416 years -748638078 days -00:00:00.000001"),
        None,
        Some("-65617554 years -6 mons +748638080 days"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "69960025 years 225766652 days -00:00:00.000001",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("69960026 years 11 mons 225766650 days -2562047788:00:54.775808"),
        Some("-69960023 years -1 mons -225766654 days -2562047788:00:54.775806"),
    ),
    (
        "-178956970 years -7 mons +00:00:00.99",
        "2 years 11 mons 2147483647 days",
        Some("178956970 years 7 mons -00:00:00.99"),
        Some("-178956967 years -8 mons +2147483647 days 00:00:00.99"),
        None,
    ),
    (
        "-1 years -1 mons +00:00:00.999999",
        "25825876 years 10 mons 311098722 days 00:00:00.000001",
        Some("1 year 1 mon -00:00:00.999999"),
        Some("25825875 years 9 mons 311098722 days 00:00:01"),
        Some("-25825877 years -11 mons -311098722 days +00:00:00.999998"),
    ),
    (
        "-1 mons +2562047788:00:54.775807",
        "-111383091 years -11 mons -679948204 days -60117069:49:08.055979",
        Some("1 mon -2562047788:00:54.775807"),
        Some("-111383092 years -679948204 days +2501930718:11:46.719828"),
        None,
    ),
    (
        "-1 years -1 mons +2 days 01:00:00",
        "2147483646 days -00:00:00.999999",
        Some("1 year 1 mon -2 days -01:00:00"),
        None,
        Some("-1 years -1 mons -2147483644 days +01:00:00.999999"),
    ),
    (
        "-1 mons +2 days -00:00:00.000001",
        "-1 years -1 mons +1 day 01:00:00",
        Some("1 mon -2 days +00:00:00.000001"),
        Some("-1 years -2 mons +3 days 00:59:59.999999"),
        Some("1 year 1 day -01:00:00.000001"),
    ),
    (
        "-1 years -1 mons +2 days -00:00:00.999999",
        "1 year 2147483647 days -2562047788:00:54.775806",
        Some("1 year 1 mon -2 days +00:00:00.999999"),
        None,
        Some("-2 years -1 mons -2147483645 days +2562047788:00:53.775807"),
    ),
    (
        "-1 years -1 days -00:00:00.000001",
        "113339416 years 748638078 days 00:00:00.000001",
        Some("1 year 1 day 00:00:00.000001"),
        Some("113339415 years 748638077 days"),
        Some("-113339417 years -748638079 days -00:00:00.000002"),
    ),
    (
        "-2 years -11 mons -2 days -01:00:00",
        "1 mon 2147483647 days 00:00:00.000001",
        Some("2 years 11 mons 2 days 01:00:00"),
        Some("-2 years -10 mons +2147483645 days -00:59:59.999999"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days -00:00:00.999999",
        "2 years 11 mons -2 days +00:00:00.000001",
        Some("-178956970 years -6 mons -2147483647 days +00:00:00.999999"),
        None,
        None,
    ),
    (
        "-1 days +2562047788:00:54.775806",
        "1 mon -01:00:00",
        Some("1 day -2562047788:00:54.775806"),
        Some("1 mon -1 days +2562047787:00:54.775806"),
        None,
    ),
    (
        "1 mon 2147483647 days 01:00:00",
        "-1 years +2147483646 days",
        Some("-1 mons -2147483647 days -01:00:00"),
        None,
        Some("1 year 1 mon 1 day 01:00:00"),
    ),
    (
        "-178956970 years -7 mons +2 days",
        "1 mon -2 days -2562047788:00:54.775806",
        Some("178956970 years 7 mons -2 days"),
        Some("-178956970 years -6 mons -2562047788:00:54.775806"),
        Some("-178956970 years -8 mons +4 days 2562047788:00:54.775806"),
    ),
    (
        "-178956970 years -8 mons +2 days 00:00:00.000001",
        "116576768 years 2 mons 590456137 days -00:00:00.072544",
        None,
        Some("-62380202 years -6 mons +590456139 days -00:00:00.072543"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483648 days +00:00:00.000001",
        "1 year 11 mons 2147483646 days 01:00:00",
        None,
        Some("-178956968 years -9 mons -2 days +01:00:00.000001"),
        None,
    ),
    (
        "84311177 years 8 mons -67678558 days -00:00:00.000001",
        "1 year -2147483648 days +2562047788:00:54.775807",
        Some("-84311177 years -8 mons +67678558 days 00:00:00.000001"),
        None,
        Some("84311176 years 8 mons 2079805090 days -2562047788:00:54.775808"),
    ),
    (
        "1 year 2147483647 days 00:00:00.00009",
        "1 year 1 mon 2147483647 days -00:00:00.999999",
        Some("-1 years -2147483647 days -00:00:00.00009"),
        None,
        Some("-1 mons +00:00:01.000089"),
    ),
    (
        "-178956970 years -8 mons +1 day 01:00:00",
        "-1 years -1 mons +2 days 00:00:00.99",
        None,
        None,
        Some("-178956969 years -7 mons -1 days +00:59:59.01"),
    ),
    (
        "1 year -1 days",
        "-1 years +2147483647 days -00:00:00.000001",
        Some("-1 years +1 day"),
        Some("2147483646 days -00:00:00.000001"),
        Some("2 years -2147483648 days +00:00:00.000001"),
    ),
    (
        "-2 years -11 mons -2147483648 days +01:00:00",
        "-1 mons -2 days +2562047788:00:54.775807",
        None,
        None,
        Some("-2 years -10 mons -2147483646 days -2562047787:00:54.775807"),
    ),
    (
        "-11 mons +00:00:00.00009",
        "-2147483648 days +00:00:00.999999",
        Some("11 mons -00:00:00.00009"),
        Some("-11 mons -2147483648 days +00:00:01.000089"),
        None,
    ),
    (
        "-178956970 years -8 mons -2562047788:00:54.775806",
        "-178956970 years -8 mons -2 days +2562047788:00:54.775806",
        None,
        None,
        None,
    ),
    (
        "178956970 years 6 mons -1 days -01:00:00",
        "11 mons -1 days +00:00:00.000001",
        Some("-178956970 years -6 mons +1 day 01:00:00"),
        None,
        Some("178956969 years 7 mons -01:00:00.000001"),
    ),
    (
        "1 year 1 mon -2147483648 days +00:00:00.00009",
        "178956970 years 7 mons -2 days",
        None,
        None,
        Some("-178956969 years -6 mons -2147483646 days +00:00:00.00009"),
    ),
    (
        "-178956970 years -8 mons -2147483648 days +2562047788:00:54.775806",
        "2 years 2147483647 days -2562047788:00:54.775806",
        None,
        Some("-178956968 years -8 mons -1 days"),
        None,
    ),
    (
        "-2 days -2562047788:00:54.775807",
        "2 years 11 mons -2 days +2562047788:00:54.775806",
        Some("2 days 2562047788:00:54.775807"),
        Some("2 years 11 mons -4 days -00:00:00.000001"),
        None,
    ),
    (
        "-75975293 years -6 mons -276952558 days +00:00:00.000001",
        "63251895 years 1 mon -190547892 days +51286344:28:08.086",
        Some("75975293 years 6 mons 276952558 days -00:00:00.000001"),
        Some("-12723398 years -5 mons -467500450 days +51286344:28:08.086001"),
        Some("-139227188 years -7 mons -86404666 days -51286344:28:08.085999"),
    ),
    (
        "1 year 11 mons -1 days +00:00:00.999999",
        "2 years 2 days 00:00:00.000001",
        Some("-1 years -11 mons +1 day -00:00:00.999999"),
        Some("3 years 11 mons 1 day 00:00:01"),
        Some("-1 mons -3 days +00:00:00.999998"),
    ),
    (
        "30619990 years 887068314 days 00:00:00.139333",
        "-1 days -00:00:00.999999",
        Some("-30619990 years -887068314 days -00:00:00.139333"),
        Some("30619990 years 887068313 days -00:00:00.860666"),
        Some("30619990 years 887068315 days 00:00:01.139332"),
    ),
    (
        "11 mons -2147483648 days +00:00:00.999999",
        "-53952014 years -10 mons +461876430 days 00:00:00.846057",
        None,
        Some("-53952013 years -11 mons -1685607218 days +00:00:01.846056"),
        None,
    ),
    (
        "-1 mons +2147483646 days -00:00:00.000001",
        "-106400853 years -10 mons +121587481 days 00:00:00.103562",
        Some("1 mon -2147483646 days +00:00:00.000001"),
        None,
        Some("106400853 years 9 mons 2025896165 days -00:00:00.103563"),
    ),
    (
        "2 years 11 mons 2 days 01:00:00",
        "-61100151 years -9 mons -684523665 days",
        Some("-2 years -11 mons -2 days -01:00:00"),
        Some("-61100148 years -10 mons -684523663 days +01:00:00"),
        Some("61100154 years 8 mons 684523667 days 01:00:00"),
    ),
    (
        "1 year 1 mon -2 days -00:00:00.000001",
        "-27520372 years -4 mons +771996111 days 00:00:00.28859",
        Some("-1 years -1 mons +2 days 00:00:00.000001"),
        Some("-27520371 years -3 mons +771996109 days 00:00:00.288589"),
        Some("27520373 years 5 mons -771996113 days -00:00:00.288591"),
    ),
    (
        "2 years -2147483648 days +2562047788:00:54.775807",
        "178956970 years 6 mons 1 day 2562047788:00:54.775806",
        None,
        None,
        None,
    ),
    (
        "1 mon -2 days",
        "28062811 years 1 mon 504038583 days",
        Some("-1 mons +2 days"),
        Some("28062811 years 2 mons 504038581 days"),
        Some("-28062811 years -504038585 days"),
    ),
    (
        "4788787 years 2 mons -833366338 days -13981230:39:04.524397",
        "-11 mons -1 days +01:00:00",
        Some("-4788787 years -2 mons +833366338 days 13981230:39:04.524397"),
        Some("4788786 years 3 mons -833366339 days -13981229:39:04.524397"),
        Some("4788788 years 1 mon -833366337 days -13981231:39:04.524397"),
    ),
    (
        "-2 years -11 mons -1 days +2562047788:00:54.775807",
        "1 mon 2147483646 days 00:00:00.000001",
        Some("2 years 11 mons 1 day -2562047788:00:54.775807"),
        None,
        Some("-3 years -2147483647 days +2562047788:00:54.775806"),
    ),
    (
        "-2 years -11 mons +2147483647 days -2562047788:00:54.775806",
        "1 year -2 days +00:00:00.999999",
        Some("2 years 11 mons -2147483647 days +2562047788:00:54.775806"),
        Some("-1 years -11 mons +2147483645 days -2562047788:00:53.775807"),
        None,
    ),
    (
        "-1 mons +2 days -00:00:00.000001",
        "-35267136 years -11 mons +211668362 days -00:00:00.047756",
        Some("1 mon -2 days +00:00:00.000001"),
        Some("-35267137 years +211668364 days -00:00:00.047757"),
        Some("35267136 years 10 mons -211668360 days +00:00:00.047755"),
    ),
    (
        "-178956970 years -8 mons +2147483647 days 00:00:00.99",
        "-178956970 years -7 mons +2147483646 days -2562047788:00:54.775806",
        None,
        None,
        None,
    ),
    (
        "-2 days +00:00:00.99",
        "11 mons -1 days -00:00:00.999999",
        Some("2 days -00:00:00.99"),
        Some("11 mons -3 days -00:00:00.009999"),
        Some("-11 mons -1 days +00:00:01.989999"),
    ),
    (
        "-11 mons +2147483647 days -2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483648 days +00:00:00.00009",
        Some("11 mons -2147483647 days +2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "1 year 11 mons -2 days",
        "2 days 00:00:00.000001",
        Some("-1 years -11 mons +2 days"),
        Some("1 year 11 mons 00:00:00.000001"),
        Some("1 year 11 mons -4 days -00:00:00.000001"),
    ),
    (
        "-42600465 years -6 mons +599662318 days -00:00:00.000001",
        "2 years 11 mons 2 days 00:00:00.999999",
        Some("42600465 years 6 mons -599662318 days +00:00:00.000001"),
        Some("-42600462 years -7 mons +599662320 days 00:00:00.999998"),
        Some("-42600468 years -5 mons +599662316 days -00:00:01"),
    ),
    (
        "1 year 1 mon 2 days -00:00:00.000001",
        "1 year 11 mons 1 day 2562047788:00:54.775806",
        Some("-1 years -1 mons -2 days +00:00:00.000001"),
        Some("3 years 3 days 2562047788:00:54.775805"),
        Some("-10 mons +1 day -2562047788:00:54.775807"),
    ),
    (
        "2 days -00:00:00.000001",
        "1 year 11 mons 1 day -00:00:00.000001",
        Some("-2 days +00:00:00.000001"),
        Some("1 year 11 mons 3 days -00:00:00.000002"),
        Some("-1 years -11 mons +1 day"),
    ),
    (
        "25029733 years 9 mons -743699285 days",
        "11 mons -1 days +00:00:00.999999",
        Some("-25029733 years -9 mons +743699285 days"),
        Some("25029734 years 8 mons -743699286 days +00:00:00.999999"),
        Some("25029732 years 10 mons -743699284 days -00:00:00.999999"),
    ),
    (
        "1 year -2 days -00:00:00.000001",
        "1 year 11 mons 2147483646 days 00:00:00.000001",
        Some("-1 years +2 days 00:00:00.000001"),
        Some("2 years 11 mons 2147483644 days"),
        Some("-11 mons -2147483648 days -00:00:00.000002"),
    ),
    (
        "1 year 11 mons -00:00:00.000001",
        "1 year 2147483646 days -00:00:00.999999",
        Some("-1 years -11 mons +00:00:00.000001"),
        Some("2 years 11 mons 2147483646 days -00:00:01"),
        Some("11 mons -2147483646 days +00:00:00.999998"),
    ),
    (
        "58769346 years 7 mons -595584827 days +00:00:00.500273",
        "1 year 1 day -2562047788:00:54.775807",
        Some("-58769346 years -7 mons +595584827 days -00:00:00.500273"),
        Some("58769347 years 7 mons -595584826 days -2562047788:00:54.275534"),
        None,
    ),
    (
        "-178956970 years -8 mons +1 day 00:00:00.999999",
        "-8023963 years -5 mons -60582205 days",
        None,
        None,
        Some("-170933007 years -3 mons +60582206 days 00:00:00.999999"),
    ),
    (
        "178956970 years 7 mons 2 days",
        "1 year 1 mon -2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2 days"),
        None,
        Some("178956969 years 6 mons 2 days 2562047788:00:54.775807"),
    ),
    (
        "2 years 11 mons -2147483648 days -2562047788:00:54.775806",
        "11 mons 2147483646 days 01:00:00",
        None,
        Some("3 years 10 mons -2 days -2562047787:00:54.775806"),
        None,
    ),
    (
        "2 days 00:00:00.999999",
        "4644036 years 5 mons -202097153 days -00:00:00.000001",
        Some("-2 days -00:00:00.999999"),
        Some("4644036 years 5 mons -202097151 days +00:00:00.999998"),
        Some("-4644036 years -5 mons +202097155 days 00:00:01"),
    ),
    (
        "1 year 1 mon 1 day 00:00:00.00009",
        "-178956970 years -8 mons +2 days -2562047788:00:54.775807",
        Some("-1 years -1 mons -1 days -00:00:00.00009"),
        Some("-178956969 years -7 mons +3 days -2562047788:00:54.775717"),
        None,
    ),
    (
        "11 mons 1 day -00:00:00.000001",
        "-2 years -11 mons +2 days -01:00:00",
        Some("-11 mons -1 days +00:00:00.000001"),
        Some("-2 years +3 days -01:00:00.000001"),
        Some("3 years 10 mons -1 days +00:59:59.999999"),
    ),
    (
        "2 years 11 mons -2 days -2562047788:00:54.775806",
        "-11 mons +2147483646 days -2562047788:00:54.775807",
        Some("-2 years -11 mons +2 days 2562047788:00:54.775806"),
        None,
        Some("3 years 10 mons -2147483648 days +00:00:00.000001"),
    ),
    (
        "-88834117 years -3 mons -876474281 days +00:00:00.000001",
        "1 year 1 mon 1 day 2562047788:00:54.775806",
        Some("88834117 years 3 mons 876474281 days -00:00:00.000001"),
        Some("-88834116 years -2 mons -876474280 days +2562047788:00:54.775807"),
        Some("-88834118 years -4 mons -876474282 days -2562047788:00:54.775805"),
    ),
    (
        "2 years -2147483648 days +2562047788:00:54.775807",
        "-53186059 years -11 mons -35039107 days -00:00:00.798555",
        None,
        None,
        None,
    ),
    (
        "178956970 years 7 mons -2 days -2562047788:00:54.775807",
        "2 years 2147483646 days 2562047788:00:54.775806",
        Some("-178956970 years -7 mons +2 days 2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-1 mons +2147483647 days 00:00:00.999999",
        "114240140 years 1 mon 730473134 days -00:00:00.189613",
        Some("1 mon -2147483647 days -00:00:00.999999"),
        None,
        Some("-114240140 years -2 mons +1417010513 days 00:00:01.189612"),
    ),
    (
        "2147483646 days 2562047788:00:54.775806",
        "2 years 11 mons -2 days +2562047788:00:54.775806",
        Some("-2147483646 days -2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "1 year 11 mons -2 days +00:00:00.999999",
        "32048527 years 3 mons 819013954 days 00:00:00.000001",
        Some("-1 years -11 mons +2 days -00:00:00.999999"),
        Some("32048529 years 2 mons 819013952 days 00:00:01"),
        Some("-32048525 years -4 mons -819013956 days +00:00:00.999998"),
    ),
    (
        "1 year 11 mons 2 days -01:00:00",
        "1 year -2147483648 days -01:00:00",
        Some("-1 years -11 mons -2 days +01:00:00"),
        Some("2 years 11 mons -2147483646 days -02:00:00"),
        None,
    ),
    (
        "178956970 years 6 mons -2147483648 days +01:00:00",
        "1 mon 2147483647 days 00:00:00.000001",
        None,
        Some("178956970 years 7 mons -1 days +01:00:00.000001"),
        None,
    ),
    (
        "-178956970 years -7 mons +2147483646 days 2562047788:00:54.775807",
        "1 year 2147483646 days 2562047788:00:54.775807",
        Some("178956970 years 7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-11 mons -1 days -2562047788:00:54.775806",
        "-116590888 years -3 mons -879628170 days",
        Some("11 mons 1 day 2562047788:00:54.775806"),
        Some("-116590889 years -2 mons -879628171 days -2562047788:00:54.775806"),
        Some("116590887 years 4 mons 879628169 days -2562047788:00:54.775806"),
    ),
    (
        "11 mons -2147483648 days -01:00:00",
        "26557822 years 5 mons 824904231 days",
        None,
        Some("26557823 years 4 mons -1322579417 days -01:00:00"),
        None,
    ),
    (
        "178956970 years 6 mons -2 days",
        "2 years -2147483648 days",
        Some("-178956970 years -6 mons +2 days"),
        None,
        Some("178956968 years 6 mons 2147483646 days"),
    ),
    (
        "71960732 years 8 mons 4986223 days 4819298:53:54.081626",
        "-2 years -11 mons +2147483647 days 00:00:00.000001",
        Some("-71960732 years -8 mons -4986223 days -4819298:53:54.081626"),
        None,
        Some("71960735 years 7 mons -2142497424 days +4819298:53:54.081625"),
    ),
    (
        "1 year 11 mons 1 day -2562047788:00:54.775807",
        "1 year 2 days",
        Some("-1 years -11 mons -1 days +2562047788:00:54.775807"),
        Some("2 years 11 mons 3 days -2562047788:00:54.775807"),
        Some("11 mons -1 days -2562047788:00:54.775807"),
    ),
    (
        "-1 years -2 days -2562047788:00:54.775807",
        "1 year 11 mons 2147483647 days -01:00:00",
        Some("1 year 2 days 2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "1 year 1 mon 2147483646 days 00:00:00.999999",
        "-1 years -2147483648 days -00:00:00.999999",
        Some("-1 years -1 mons -2147483646 days -00:00:00.999999"),
        Some("1 mon -2 days"),
        None,
    ),
    (
        "-11 mons +2147483646 days 01:00:00",
        "1 year 1 mon -1 days +00:00:00.99",
        Some("11 mons -2147483646 days -01:00:00"),
        Some("2 mons 2147483645 days 01:00:00.99"),
        Some("-2 years +2147483647 days 00:59:59.01"),
    ),
    (
        "1 day 2562047788:00:54.775806",
        "2 years 1 day 00:00:00.999999",
        Some("-1 days -2562047788:00:54.775806"),
        None,
        Some("-2 years +2562047788:00:53.775807"),
    ),
    (
        "-102686030 years -1 mons +717345364 days 00:00:00.000001",
        "1 year 11 mons 2 days -00:00:00.999999",
        Some("102686030 years 1 mon -717345364 days -00:00:00.000001"),
        Some("-102686028 years -2 mons +717345366 days -00:00:00.999998"),
        Some("-102686032 years +717345362 days 00:00:01"),
    ),
    (
        "-2 years -11 mons +2147483646 days 00:00:00.99",
        "-11 mons +2147483647 days 00:00:00.99",
        Some("2 years 11 mons -2147483646 days -00:00:00.99"),
        None,
        Some("-2 years -1 days"),
    ),
    (
        "1 year -2147483648 days +2562047788:00:54.775806",
        "-71983076 years -4 mons +668098513 days -00:00:00.000001",
        None,
        Some("-71983075 years -4 mons -1479385135 days +2562047788:00:54.775805"),
        None,
    ),
    (
        "2 years 00:00:00.000001",
        "-1 mons +2 days 00:00:00.00009",
        Some("-2 years -00:00:00.000001"),
        Some("1 year 11 mons 2 days 00:00:00.000091"),
        Some("2 years 1 mon -2 days -00:00:00.000089"),
    ),
    (
        "-11 mons -1 days +00:00:00.999999",
        "178956970 years 7 mons 2147483646 days 00:00:00.999999",
        Some("11 mons 1 day -00:00:00.999999"),
        Some("178956969 years 8 mons 2147483645 days 00:00:01.999998"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483646 days -01:00:00",
        "-1 years -1 mons -2 days",
        Some("-178956970 years -6 mons -2147483646 days +01:00:00"),
        Some("178956969 years 5 mons 2147483644 days -01:00:00"),
        None,
    ),
    (
        "-178956970 years -8 mons -1 days -2562047788:00:54.775807",
        "-2 years -11 mons +1 day 2562047788:00:54.775807",
        None,
        None,
        None,
    ),
    (
        "-178956970 years -8 mons +2147483646 days -00:00:00.000001",
        "1 year -2147483648 days +00:00:00.00009",
        None,
        Some("-178956969 years -8 mons -2 days +00:00:00.000089"),
        None,
    ),
    (
        "2 years 11 mons -1 days +01:00:00",
        "-33022774 years -10 mons +3693968 days -00:00:00.000001",
        Some("-2 years -11 mons +1 day -01:00:00"),
        Some("-33022771 years -11 mons +3693967 days 00:59:59.999999"),
        Some("33022777 years 9 mons -3693969 days +01:00:00.000001"),
    ),
    (
        "28062811 years 1 mon 504038583 days",
        "-11 mons -2562047788:00:54.775806",
        Some("-28062811 years -1 mons -504038583 days"),
        Some("28062810 years 2 mons 504038583 days -2562047788:00:54.775806"),
        Some("28062812 years 504038583 days 2562047788:00:54.775806"),
    ),
    (
        "-178956970 years -8 mons -1 days +00:00:00.000001",
        "11 mons 2147483646 days -00:00:00.000001",
        None,
        Some("-178956969 years -9 mons +2147483645 days"),
        None,
    ),
    (
        "-2 years -11 mons +2 days 00:00:00.000001",
        "-83906929 years -7 mons -248240121 days +00:00:00.627253",
        Some("2 years 11 mons -2 days -00:00:00.000001"),
        Some("-83906932 years -6 mons -248240119 days +00:00:00.627254"),
        Some("83906926 years 8 mons 248240123 days -00:00:00.627252"),
    ),
    (
        "1 year 11 mons 2147483647 days -00:00:00.999999",
        "6610413 years 7 mons 637009719 days",
        Some("-1 years -11 mons -2147483647 days +00:00:00.999999"),
        None,
        Some("-6610411 years -8 mons +1510473928 days -00:00:00.999999"),
    ),
    (
        "1 year -2147483648 days",
        "-1 mons -2147483648 days +00:00:00.999999",
        None,
        None,
        Some("1 year 1 mon -00:00:00.999999"),
    ),
    (
        "-1 years -1 mons +1 day 00:00:00.999999",
        "178956970 years 7 mons 2147483646 days -00:00:00.999999",
        Some("1 year 1 mon -1 days -00:00:00.999999"),
        Some("178956969 years 6 mons 2147483647 days"),
        None,
    ),
    (
        "-108478710 years -10 mons +376832893 days 00:00:00.000001",
        "1 mon 2562047788:00:54.775807",
        Some("108478710 years 10 mons -376832893 days -00:00:00.000001"),
        None,
        Some("-108478710 years -11 mons +376832893 days -2562047788:00:54.775806"),
    ),
    (
        "-178956970 years -7 mons +2147483647 days 00:00:00.00009",
        "2 years 2 days -2562047788:00:54.775806",
        Some("178956970 years 7 mons -2147483647 days -00:00:00.00009"),
        None,
        None,
    ),
    (
        "-1 years -1 mons +1 day 2562047788:00:54.775807",
        "1 year 2 days -01:00:00",
        Some("1 year 1 mon -1 days -2562047788:00:54.775807"),
        Some("-1 mons +3 days 2562047787:00:54.775807"),
        None,
    ),
    (
        "-22741005 years -10 mons -112195665 days -00:00:00.502913",
        "-2 days +00:00:00.99",
        Some("22741005 years 10 mons 112195665 days 00:00:00.502913"),
        Some("-22741005 years -10 mons -112195667 days +00:00:00.487087"),
        Some("-22741005 years -10 mons -112195663 days -00:00:01.492913"),
    ),
    (
        "-3964874 years -3 mons +742200724 days",
        "-178956970 years -8 mons +2147483646 days 2562047788:00:54.775807",
        Some("3964874 years 3 mons -742200724 days"),
        None,
        Some("174992096 years 5 mons -1405282922 days -2562047788:00:54.775807"),
    ),
    (
        "1 mon 00:00:00.99",
        "-21144227 years -4 mons +884901852 days -00:00:00.000001",
        Some("-1 mons -00:00:00.99"),
        Some("-21144227 years -3 mons +884901852 days 00:00:00.989999"),
        Some("21144227 years 5 mons -884901852 days +00:00:00.990001"),
    ),
    (
        "178956970 years 7 mons -1 days",
        "178956970 years 7 mons 2 days 2562047788:00:54.775807",
        Some("-178956970 years -7 mons +1 day"),
        None,
        Some("-3 days -2562047788:00:54.775807"),
    ),
    (
        "1 year -2 days +2562047788:00:54.775807",
        "-2 years -11 mons +2 days -2562047788:00:54.775807",
        Some("-1 years +2 days -2562047788:00:54.775807"),
        Some("-1 years -11 mons"),
        None,
    ),
    (
        "1 year -2 days +00:00:00.000001",
        "120920644 years 10 mons -598162517 days +00:00:00.000001",
        Some("-1 years +2 days -00:00:00.000001"),
        Some("120920645 years 10 mons -598162519 days +00:00:00.000002"),
        Some("-120920643 years -10 mons +598162515 days"),
    ),
    (
        "-111981005 years -6 mons -228802805 days",
        "-178956970 years -8 mons +2147483647 days -00:00:00.999999",
        Some("111981005 years 6 mons 228802805 days"),
        None,
        None,
    ),
    (
        "1 year 1 mon -2147483648 days +00:00:00.000001",
        "-121141604 years -5 mons -688582191 days",
        None,
        None,
        Some("121141605 years 6 mons -1458901457 days +00:00:00.000001"),
    ),
    (
        "178956970 years 7 mons 1 day 01:00:00",
        "1 year 1 mon 2147483646 days 00:00:00.00009",
        Some("-178956970 years -7 mons -1 days -01:00:00"),
        None,
        Some("178956969 years 6 mons -2147483645 days +00:59:59.99991"),
    ),
    (
        "32653935 years 8 mons 790345379 days 00:00:00.838767",
        "1 day 00:00:00.00009",
        Some("-32653935 years -8 mons -790345379 days -00:00:00.838767"),
        Some("32653935 years 8 mons 790345380 days 00:00:00.838857"),
        Some("32653935 years 8 mons 790345378 days 00:00:00.838677"),
    ),
    (
        "-1 mons -1 days +00:00:00.99",
        "-1 mons +00:00:00.000001",
        Some("1 mon 1 day -00:00:00.99"),
        Some("-2 mons -1 days +00:00:00.990001"),
        Some("-1 days +00:00:00.989999"),
    ),
    (
        "1 year 11 mons 2147483646 days 00:00:00.00009",
        "-1 years +2 days 2562047788:00:54.775807",
        Some("-1 years -11 mons -2147483646 days -00:00:00.00009"),
        None,
        Some("2 years 11 mons 2147483644 days -2562047788:00:54.775717"),
    ),
    (
        "-1 mons +2147483646 days 00:00:00.999999",
        "2 years 11 mons 00:00:00.00009",
        Some("1 mon -2147483646 days -00:00:00.999999"),
        Some("2 years 10 mons 2147483646 days 00:00:01.000089"),
        Some("-3 years +2147483646 days 00:00:00.999909"),
    ),
    (
        "-1 years -1 mons +1 day -00:00:00.000001",
        "-1 mons -2 days +00:00:00.999999",
        Some("1 year 1 mon -1 days +00:00:00.000001"),
        Some("-1 years -2 mons -1 days +00:00:00.999998"),
        Some("-1 years +3 days -00:00:01"),
    ),
    (
        "-1 years +1 day 01:00:00",
        "-2 years -11 mons +2147483647 days 2562047788:00:54.775807",
        Some("1 year -1 days -01:00:00"),
        None,
        Some("1 year 11 mons -2147483646 days -2562047787:00:54.775807"),
    ),
    (
        "178956970 years 7 mons -2562047788:00:54.775807",
        "-1 years -1 mons +2147483647 days -01:00:00",
        Some("-178956970 years -7 mons +2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "2 years 2147483647 days -2562047788:00:54.775807",
        "-1 years +1 day 2562047788:00:54.775807",
        Some("-2 years -2147483647 days +2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-24263353 years -11 mons +843439070 days -12479851:57:42.632695",
        "178956970 years 6 mons -1 days",
        Some("24263353 years 11 mons -843439070 days +12479851:57:42.632695"),
        Some("154693616 years 7 mons 843439069 days -12479851:57:42.632695"),
        None,
    ),
    (
        "2 years 2 days 00:00:00.99",
        "1 year 1 mon 2147483646 days -01:00:00",
        Some("-2 years -2 days -00:00:00.99"),
        None,
        Some("11 mons -2147483644 days +01:00:00.99"),
    ),
    (
        "-1 mons +1 day 01:00:00",
        "-1 days +00:00:00.000001",
        Some("1 mon -1 days -01:00:00"),
        Some("-1 mons +01:00:00.000001"),
        Some("-1 mons +2 days 00:59:59.999999"),
    ),
    (
        "-1 mons +1 day -01:00:00",
        "-1 years -1 days +00:00:00.99",
        Some("1 mon -1 days +01:00:00"),
        Some("-1 years -1 mons -00:59:59.01"),
        Some("11 mons 2 days -01:00:00.99"),
    ),
    (
        "1 year 11 mons 2147483647 days -00:00:00.999999",
        "1 year 11 mons -2 days -00:00:00.999999",
        Some("-1 years -11 mons -2147483647 days +00:00:00.999999"),
        Some("3 years 10 mons 2147483645 days -00:00:01.999998"),
        None,
    ),
    (
        "36074911 years 8 mons 851157591 days",
        "-37149938 years -7 mons +629444420 days -00:00:00.000001",
        Some("-36074911 years -8 mons -851157591 days"),
        Some("-1075026 years -11 mons +1480602011 days -00:00:00.000001"),
        Some("73224850 years 3 mons 221713171 days 00:00:00.000001"),
    ),
    (
        "-1 years -1 days +00:00:00.99",
        "1 year 11 mons 2147483646 days -2562047788:00:54.775806",
        Some("1 year 1 day -00:00:00.99"),
        Some("11 mons 2147483645 days -2562047788:00:53.785806"),
        None,
    ),
    (
        "-2 years -11 mons -2 days",
        "1 day 00:00:00.99",
        Some("2 years 11 mons 2 days"),
        Some("-2 years -11 mons -1 days +00:00:00.99"),
        Some("-2 years -11 mons -3 days -00:00:00.99"),
    ),
    (
        "1 year 11 mons 1 day 2562047788:00:54.775806",
        "-1 years -1 mons -1 days +00:00:00.99",
        Some("-1 years -11 mons -1 days -2562047788:00:54.775806"),
        None,
        Some("3 years 2 days 2562047788:00:53.785806"),
    ),
    (
        "11 mons -2 days +00:00:00.99",
        "-1 years -1 days -01:00:00",
        Some("-11 mons +2 days -00:00:00.99"),
        Some("-1 mons -3 days -00:59:59.01"),
        Some("1 year 11 mons -1 days +01:00:00.99"),
    ),
    (
        "-121608484 years -8 mons -318161962 days -00:00:00.000001",
        "1 year -2 days -01:00:00",
        Some("121608484 years 8 mons 318161962 days 00:00:00.000001"),
        Some("-121608483 years -8 mons -318161964 days -01:00:00.000001"),
        Some("-121608485 years -8 mons -318161960 days +00:59:59.999999"),
    ),
    (
        "2 years 11 mons 2 days -01:00:00",
        "2147483646 days 2562047788:00:54.775806",
        Some("-2 years -11 mons -2 days +01:00:00"),
        None,
        None,
    ),
    (
        "2 years -2 days -2562047788:00:54.775806",
        "-1 years -1 mons +2 days -2562047788:00:54.775807",
        Some("-2 years +2 days 2562047788:00:54.775806"),
        None,
        Some("3 years 1 mon -4 days +00:00:00.000001"),
    ),
    (
        "178956970 years 6 mons -2 days +00:00:00.000001",
        "15844936 years 3 mons 800661521 days 00:00:00.000001",
        Some("-178956970 years -6 mons +2 days -00:00:00.000001"),
        None,
        Some("163112034 years 3 mons -800661523 days"),
    ),
    (
        "2 years 11 mons -2147483648 days -2562047788:00:54.775807",
        "1 mon 01:00:00",
        None,
        Some("3 years -2147483648 days -2562047787:00:54.775807"),
        None,
    ),
    (
        "2 years 11 mons 1 day -2562047788:00:54.775806",
        "1 mon 2 days 00:00:00.000001",
        Some("-2 years -11 mons -1 days +2562047788:00:54.775806"),
        Some("3 years 3 days -2562047788:00:54.775805"),
        Some("2 years 10 mons -1 days -2562047788:00:54.775807"),
    ),
    (
        "2 years 11 mons -2147483648 days +2562047788:00:54.775806",
        "-114682152 years -4 mons -499795150 days +00:00:00.000001",
        None,
        None,
        Some("114682155 years 3 mons -1647688498 days +2562047788:00:54.775805"),
    ),
    (
        "-2 years -11 mons +2147483647 days 2562047788:00:54.775807",
        "-20881798 years -5 mons -69968499 days -00:00:00.552255",
        Some("2 years 11 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-20881801 years -4 mons +2077515148 days 2562047788:00:54.223552"),
        None,
    ),
    (
        "2 years -1 days -01:00:00",
        "2 years 2147483647 days 00:00:00.99",
        Some("-2 years +1 day 01:00:00"),
        Some("4 years 2147483646 days -00:59:59.01"),
        Some("-2147483648 days -01:00:00.99"),
    ),
    (
        "-11 mons +2147483647 days -00:00:00.999999",
        "70056143 years 1 mon -242228296 days +00:00:00.971829",
        Some("11 mons -2147483647 days +00:00:00.999999"),
        Some("70056142 years 2 mons 1905255351 days -00:00:00.02817"),
        None,
    ),
    (
        "-84244857 years +180073582 days -00:00:00.000001",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("84244857 years -180073582 days +00:00:00.000001"),
        Some("-84244855 years -1 mons +180073580 days -2562047788:00:54.775808"),
        Some("-84244858 years -11 mons +180073584 days 2562047788:00:54.775806"),
    ),
    (
        "2 years -1 days -00:00:00.000001",
        "1 year 1 mon 2147483646 days -01:00:00",
        Some("-2 years +1 day 00:00:00.000001"),
        Some("3 years 1 mon 2147483645 days -01:00:00.000001"),
        Some("11 mons -2147483647 days +00:59:59.999999"),
    ),
    (
        "-11 mons +2147483646 days 00:00:00.000001",
        "-2 days +00:00:00.999999",
        Some("11 mons -2147483646 days -00:00:00.000001"),
        Some("-11 mons +2147483644 days 00:00:01"),
        None,
    ),
    (
        "178956970 years 7 mons -2 days +01:00:00",
        "65295955 years 1 mon -526504316 days -00:00:00.000001",
        Some("-178956970 years -7 mons +2 days -01:00:00"),
        None,
        Some("113661015 years 6 mons 526504314 days 01:00:00.000001"),
    ),
    (
        "-178956970 years -7 mons +1 day 00:00:00.999999",
        "-2 years -11 mons +1 day 01:00:00",
        Some("178956970 years 7 mons -1 days -00:00:00.999999"),
        None,
        Some("-178956967 years -8 mons -00:59:59.000001"),
    ),
    (
        "-178956970 years -8 mons +2147483646 days 00:00:00.999999",
        "-112735050 years -1 mons +559975346 days 11366796:56:30.659267",
        None,
        None,
        Some("-66221920 years -7 mons +1587508300 days -11366796:56:29.659268"),
    ),
    (
        "1 year 11 mons -2 days +00:00:00.00009",
        "-2 years -11 mons -2 days +00:00:00.999999",
        Some("-1 years -11 mons +2 days -00:00:00.00009"),
        Some("-1 years -4 days +00:00:01.000089"),
        Some("4 years 10 mons -00:00:00.999909"),
    ),
    (
        "178956970 years 7 mons 2147483646 days",
        "-1 years +00:00:00.99",
        Some("-178956970 years -7 mons -2147483646 days"),
        Some("178956969 years 7 mons 2147483646 days 00:00:00.99"),
        None,
    ),
    (
        "-178956970 years -8 mons -2 days",
        "-264470 years -8 mons +183876337 days",
        None,
        None,
        Some("-178692500 years -183876339 days"),
    ),
    (
        "-178956970 years -7 mons +2 days",
        "-1 mons +1 day -2562047788:00:54.775807",
        Some("178956970 years 7 mons -2 days"),
        Some("-178956970 years -8 mons +3 days -2562047788:00:54.775807"),
        Some("-178956970 years -6 mons +1 day 2562047788:00:54.775807"),
    ),
    (
        "1 mon 2147483646 days -00:00:00.999999",
        "1 year 1 mon -2 days +00:00:00.00009",
        Some("-1 mons -2147483646 days +00:00:00.999999"),
        Some("1 year 2 mons 2147483644 days -00:00:00.999909"),
        None,
    ),
    (
        "-1 years +1 day -01:00:00",
        "-11 mons +2 days -2562047788:00:54.775806",
        Some("1 year -1 days +01:00:00"),
        None,
        Some("-1 mons -1 days +2562047787:00:54.775806"),
    ),
    (
        "1 year 2562047788:00:54.775806",
        "2 years 11 mons -2147483648 days +2562047788:00:54.775806",
        Some("-1 years -2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "178956970 years 6 mons -1 days +00:00:00.00009",
        "1 year -2 days -00:00:00.000001",
        Some("-178956970 years -6 mons +1 day -00:00:00.00009"),
        None,
        Some("178956969 years 6 mons 1 day 00:00:00.000091"),
    ),
    (
        "1 year 1 mon 1 day 2562047788:00:54.775806",
        "69326254 years 5 mons 413639673 days",
        Some("-1 years -1 mons -1 days -2562047788:00:54.775806"),
        Some("69326255 years 6 mons 413639674 days 2562047788:00:54.775806"),
        Some("-69326253 years -4 mons -413639672 days +2562047788:00:54.775806"),
    ),
    (
        "-1 mons -2 days -01:00:00",
        "2147483647 days -00:00:00.999999",
        Some("1 mon 2 days 01:00:00"),
        Some("-1 mons +2147483645 days -01:00:00.999999"),
        None,
    ),
    (
        "-113563449 years -10 mons +209680159 days -00:00:00.000001",
        "178956970 years 7 mons 2 days -00:00:00.000001",
        Some("113563449 years 10 mons -209680159 days +00:00:00.000001"),
        Some("65393520 years 9 mons 209680161 days -00:00:00.000002"),
        None,
    ),
    (
        "-178956970 years -8 mons -1 days -01:00:00",
        "-1 years +1 day 2562047788:00:54.775807",
        None,
        None,
        None,
    ),
    (
        "-11 mons -2147483648 days +00:00:00.000001",
        "-178956970 years -8 mons -1 days +2562047788:00:54.775807",
        None,
        None,
        Some("178956969 years 9 mons -2147483647 days -2562047788:00:54.775806"),
    ),
    (
        "-110549979 years -9 mons -550884492 days",
        "-107569204 years -562190079 days",
        Some("110549979 years 9 mons 550884492 days"),
        None,
        Some("-2980775 years -9 mons +11305587 days"),
    ),
    (
        "-75975293 years -6 mons -276952558 days +00:00:00.000001",
        "-2 years -11 mons +2147483646 days 01:00:00",
        Some("75975293 years 6 mons 276952558 days -00:00:00.000001"),
        Some("-75975296 years -5 mons +1870531088 days 01:00:00.000001"),
        None,
    ),
    (
        "-111383091 years -11 mons -679948204 days -60117069:49:08.055979",
        "-178956970 years -7 mons -00:00:00.999999",
        Some("111383091 years 11 mons 679948204 days 60117069:49:08.055979"),
        None,
        Some("67573878 years 8 mons -679948204 days -60117069:49:07.05598"),
    ),
    (
        "-11 mons -1 days +01:00:00",
        "-11 mons +2147483646 days -2562047788:00:54.775807",
        Some("11 mons 1 day -01:00:00"),
        Some("-1 years -10 mons +2147483645 days -2562047787:00:54.775807"),
        None,
    ),
    (
        "1 year 11 mons -2 days",
        "-54637773 years -2 mons +575265433 days 00:00:00.000001",
        Some("-1 years -11 mons +2 days"),
        Some("-54637771 years -3 mons +575265431 days 00:00:00.000001"),
        Some("54637775 years 1 mon -575265435 days -00:00:00.000001"),
    ),
    (
        "1 mon 2147483646 days -00:00:00.999999",
        "114523359 years 11 mons -782088963 days +00:00:00.274894",
        Some("-1 mons -2147483646 days +00:00:00.999999"),
        Some("114523360 years 1365394683 days -00:00:00.725105"),
        None,
    ),
    (
        "178956970 years 7 mons 00:00:00.99",
        "1 mon -2147483648 days +00:00:00.99",
        Some("-178956970 years -7 mons -00:00:00.99"),
        None,
        None,
    ),
    (
        "-1 mons +2 days 00:00:00.99",
        "178956970 years 6 mons 1 day 00:00:00.000001",
        Some("1 mon -2 days -00:00:00.99"),
        Some("178956970 years 5 mons 3 days 00:00:00.990001"),
        Some("-178956970 years -7 mons +1 day 00:00:00.989999"),
    ),
    (
        "8328616 years 10 mons 825964162 days -00:00:00.000001",
        "178956970 years 7 mons -2 days +2562047788:00:54.775807",
        Some("-8328616 years -10 mons -825964162 days +00:00:00.000001"),
        None,
        Some("-170628353 years -9 mons +825964164 days -2562047788:00:54.775808"),
    ),
    (
        "-11 mons +1 day",
        "36912947 years 9 mons 302102035 days -00:00:00.000001",
        Some("11 mons -1 days"),
        Some("36912946 years 10 mons 302102036 days -00:00:00.000001"),
        Some("-36912948 years -8 mons -302102034 days +00:00:00.000001"),
    ),
    (
        "2 years -2562047788:00:54.775806",
        "-29612643 years -6 mons +207965225 days -00:00:00.000001",
        Some("-2 years +2562047788:00:54.775806"),
        Some("-29612641 years -6 mons +207965225 days -2562047788:00:54.775807"),
        Some("29612645 years 6 mons -207965225 days -2562047788:00:54.775805"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-61184191 years -897067992 days",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("-61184189 years -1 mons -897067994 days -2562047788:00:54.775807"),
        Some("61184192 years 11 mons 897067990 days -2562047788:00:54.775807"),
    ),
    (
        "71960732 years 8 mons 4986223 days 4819298:53:54.081626",
        "178956970 years 7 mons 2 days",
        Some("-71960732 years -8 mons -4986223 days -4819298:53:54.081626"),
        None,
        Some("-106996237 years -11 mons +4986221 days 4819298:53:54.081626"),
    ),
    (
        "178956970 years 7 mons 2147483646 days",
        "51764879 years 10 mons 543425336 days 19259456:58:56.752156",
        Some("-178956970 years -7 mons -2147483646 days"),
        None,
        Some("127192090 years 9 mons 1604058310 days -19259456:58:56.752156"),
    ),
    (
        "-1 years -1 mons -2147483648 days +00:00:00.99",
        "-178956970 years -7 mons +00:00:00.00009",
        None,
        None,
        Some("178956969 years 6 mons -2147483648 days +00:00:00.98991"),
    ),
    (
        "1 year 11 mons 1 day 2562047788:00:54.775806",
        "-119067648 years -11 mons -751492224 days -00:00:00.065288",
        Some("-1 years -11 mons -1 days -2562047788:00:54.775806"),
        Some("-119067647 years -751492223 days +2562047788:00:54.710518"),
        None,
    ),
    (
        "178956970 years 6 mons 2 days 00:00:00.99",
        "32048527 years 3 mons 819013954 days 00:00:00.000001",
        Some("-178956970 years -6 mons -2 days -00:00:00.99"),
        None,
        Some("146908443 years 3 mons -819013952 days +00:00:00.989999"),
    ),
    (
        "-2 years -11 mons +2147483647 days 2562047788:00:54.775807",
        "1 year 1 mon -2147483648 days -2562047788:00:54.775807",
        Some("2 years 11 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-1 years -10 mons -1 days"),
        None,
    ),
    (
        "1 year 1 day 00:00:00.999999",
        "86768760 years -491925194 days +00:00:00.579888",
        Some("-1 years -1 days -00:00:00.999999"),
        Some("86768761 years -491925193 days +00:00:01.579887"),
        Some("-86768759 years +491925195 days 00:00:00.420111"),
    ),
    (
        "2 days 00:00:00.00009",
        "-1 mons +00:00:00.99",
        Some("-2 days -00:00:00.00009"),
        Some("-1 mons +2 days 00:00:00.99009"),
        Some("1 mon 2 days -00:00:00.98991"),
    ),
    (
        "-1 years +2562047788:00:54.775807",
        "-178956970 years -8 mons +2 days 00:00:00.999999",
        Some("1 year -2562047788:00:54.775807"),
        None,
        Some("178956969 years 8 mons -2 days +2562047788:00:53.775808"),
    ),
    (
        "178956970 years 7 mons -2 days -00:00:00.000001",
        "2562047788:00:54.775806",
        Some("-178956970 years -7 mons +2 days 00:00:00.000001"),
        Some("178956970 years 7 mons -2 days +2562047788:00:54.775805"),
        Some("178956970 years 7 mons -2 days -2562047788:00:54.775807"),
    ),
    (
        "178956970 years 7 mons 2 days -00:00:00.000001",
        "-178956970 years -8 mons -1 days",
        Some("-178956970 years -7 mons -2 days +00:00:00.000001"),
        Some("-1 mons +1 day -00:00:00.000001"),
        None,
    ),
    (
        "2 years 11 mons -2147483648 days +2562047788:00:54.775807",
        "-178956970 years -7 mons +2147483647 days -2562047788:00:54.775807",
        None,
        Some("-178956967 years -8 mons -1 days"),
        None,
    ),
    (
        "1 mon 2147483646 days 00:00:00.000001",
        "2147483646 days 00:00:00.000001",
        Some("-1 mons -2147483646 days -00:00:00.000001"),
        None,
        Some("1 mon"),
    ),
    (
        "-178956970 years -7 mons +2 days 2562047788:00:54.775806",
        "-1 mons +00:00:00.000001",
        Some("178956970 years 7 mons -2 days -2562047788:00:54.775806"),
        Some("-178956970 years -8 mons +2 days 2562047788:00:54.775807"),
        Some("-178956970 years -6 mons +2 days 2562047788:00:54.775805"),
    ),
    (
        "-40554342 years -6 mons -342342265 days -00:00:00.925901",
        "-11 mons +2 days",
        Some("40554342 years 6 mons 342342265 days 00:00:00.925901"),
        Some("-40554343 years -5 mons -342342263 days -00:00:00.925901"),
        Some("-40554341 years -7 mons -342342267 days -00:00:00.925901"),
    ),
    (
        "-178956970 years -8 mons -2147483648 days +00:00:00.999999",
        "178956970 years 7 mons 2147483647 days -00:00:00.999999",
        None,
        Some("-1 mons -1 days"),
        None,
    ),
    (
        "-178956970 years -8 mons +2 days -00:00:00.999999",
        "-178956970 years -7 mons +2147483647 days 00:00:00.999999",
        None,
        None,
        Some("-1 mons -2147483645 days -00:00:01.999998"),
    ),
    (
        "2 years 11 mons -2147483648 days +2562047788:00:54.775806",
        "-1 years -1 mons -2562047788:00:54.775807",
        None,
        Some("1 year 10 mons -2147483648 days -00:00:00.000001"),
        None,
    ),
    (
        "-40554342 years -6 mons -342342265 days -00:00:00.925901",
        "-1 mons +2147483647 days -2562047788:00:54.775806",
        Some("40554342 years 6 mons 342342265 days 00:00:00.925901"),
        None,
        None,
    ),
    (
        "178956970 years 6 mons 2147483646 days 2562047788:00:54.775807",
        "2 years -2147483648 days -00:00:00.999999",
        Some("-178956970 years -6 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "1 year 1 mon -01:00:00",
        "-178956970 years -8 mons -2147483648 days -00:00:00.999999",
        Some("-1 years -1 mons +01:00:00"),
        Some("-178956969 years -7 mons -2147483648 days -01:00:00.999999"),
        None,
    ),
    (
        "11 mons 2147483646 days -00:00:00.000001",
        "178956970 years 7 mons 2147483646 days 00:00:00.99",
        Some("-11 mons -2147483646 days +00:00:00.000001"),
        None,
        Some("-178956969 years -8 mons -00:00:00.990001"),
    ),
    (
        "-1 years -1 mons +2 days 00:00:00.99",
        "1 year 11 mons 2147483646 days 00:00:00.000001",
        Some("1 year 1 mon -2 days -00:00:00.99"),
        None,
        Some("-3 years -2147483644 days +00:00:00.989999"),
    ),
    (
        "-2 years -11 mons +2562047788:00:54.775807",
        "-1 mons +2 days 00:00:00.00009",
        Some("2 years 11 mons -2562047788:00:54.775807"),
        None,
        Some("-2 years -10 mons -2 days +2562047788:00:54.775717"),
    ),
    (
        "1 day 01:00:00",
        "11 mons 2147483646 days",
        Some("-1 days -01:00:00"),
        Some("11 mons 2147483647 days 01:00:00"),
        Some("-11 mons -2147483645 days +01:00:00"),
    ),
    (
        "1 year 1 mon -2562047788:00:54.775806",
        "39023322 years 7 mons -217917864 days -00:00:00.000001",
        Some("-1 years -1 mons +2562047788:00:54.775806"),
        Some("39023323 years 8 mons -217917864 days -2562047788:00:54.775807"),
        Some("-39023321 years -6 mons +217917864 days -2562047788:00:54.775805"),
    ),
    (
        "117193855 years 7 mons 492319201 days 00:00:00.000001",
        "178956970 years 7 mons -00:00:00.000001",
        Some("-117193855 years -7 mons -492319201 days -00:00:00.000001"),
        None,
        Some("-61763115 years +492319201 days 00:00:00.000002"),
    ),
    (
        "2147483647 days 00:00:00.999999",
        "-104510725 years -570320810 days -00:00:00.712502",
        Some("-2147483647 days -00:00:00.999999"),
        Some("-104510725 years +1577162837 days 00:00:00.287497"),
        None,
    ),
    (
        "-1 years -1 mons -2 days +00:00:00.00009",
        "75662389 years 10 mons -291127243 days +11475405:21:07.736612",
        Some("1 year 1 mon 2 days -00:00:00.00009"),
        Some("75662388 years 9 mons -291127245 days +11475405:21:07.736702"),
        Some("-75662390 years -11 mons +291127241 days -11475405:21:07.736522"),
    ),
    (
        "48233762 years 5 mons 127690117 days",
        "2 years 11 mons -2 days -2562047788:00:54.775807",
        Some("-48233762 years -5 mons -127690117 days"),
        Some("48233765 years 4 mons 127690115 days -2562047788:00:54.775807"),
        Some("48233759 years 6 mons 127690119 days 2562047788:00:54.775807"),
    ),
    (
        "-38792309 years -11 mons +728491668 days 00:00:00.742218",
        "-1 days -01:00:00",
        Some("38792309 years 11 mons -728491668 days -00:00:00.742218"),
        Some("-38792309 years -11 mons +728491667 days -00:59:59.257782"),
        Some("-38792309 years -11 mons +728491669 days 01:00:00.742218"),
    ),
    (
        "-11 mons -1 days -2562047788:00:54.775806",
        "1 year 1 mon 2147483646 days -00:00:00.000001",
        Some("11 mons 1 day 2562047788:00:54.775806"),
        Some("2 mons 2147483645 days -2562047788:00:54.775807"),
        Some("-2 years -2147483647 days -2562047788:00:54.775805"),
    ),
    (
        "-1 mons +00:00:00.000001",
        "-2 years -11 mons +1 day 01:00:00",
        Some("1 mon -00:00:00.000001"),
        Some("-3 years +1 day 01:00:00.000001"),
        Some("2 years 10 mons -1 days -00:59:59.999999"),
    ),
    (
        "1 year 1 mon 1 day 00:00:00.00009",
        "-1 years -2147483648 days -2562047788:00:54.775807",
        Some("-1 years -1 mons -1 days -00:00:00.00009"),
        Some("1 mon -2147483647 days -2562047788:00:54.775717"),
        None,
    ),
    (
        "32653935 years 8 mons 790345379 days 00:00:00.838767",
        "2 years 11 mons -2147483648 days",
        Some("-32653935 years -8 mons -790345379 days -00:00:00.838767"),
        Some("32653938 years 7 mons -1357138269 days +00:00:00.838767"),
        None,
    ),
    (
        "178956970 years 6 mons -2147483648 days +01:00:00",
        "-178956970 years -8 mons +2562047788:00:54.775807",
        None,
        None,
        None,
    ),
    (
        "1 year 11 mons -2147483648 days -00:00:00.999999",
        "2 years -2147483648 days -00:00:00.000001",
        None,
        None,
        Some("-1 mons -00:00:00.999998"),
    ),
    (
        "-1 years -1 mons +2147483646 days 2562047788:00:54.775806",
        "9862962 years 8 mons 281144070 days",
        Some("1 year 1 mon -2147483646 days -2562047788:00:54.775806"),
        None,
        Some("-9862963 years -9 mons +1866339576 days 2562047788:00:54.775806"),
    ),
    (
        "1 mon -2147483648 days",
        "72527967 years 6 mons -380991253 days -00:00:00.81961",
        None,
        None,
        Some("-72527967 years -5 mons -1766492395 days +00:00:00.81961"),
    ),
    (
        "78975172 years 4 mons 565406630 days -00:00:00.000001",
        "51954711 years 7 mons 699503421 days -59523373:23:52.847638",
        Some("-78975172 years -4 mons -565406630 days +00:00:00.000001"),
        Some("130929883 years 11 mons 1264910051 days -59523373:23:52.847639"),
        Some("27020460 years 9 mons -134096791 days +59523373:23:52.847637"),
    ),
    (
        "1 year 1 mon -2147483648 days +00:00:00.99",
        "178956970 years 7 mons 2147483646 days -00:00:00.999999",
        None,
        None,
        None,
    ),
    (
        "-178956970 years -8 mons +01:00:00",
        "1 year 1 mon 1 day -2562047788:00:54.775807",
        None,
        Some("-178956969 years -7 mons +1 day -2562047787:00:54.775807"),
        None,
    ),
    (
        "2 years -1 days +00:00:00.00009",
        "178956970 years 7 mons -2 days +01:00:00",
        Some("-2 years +1 day -00:00:00.00009"),
        None,
        Some("-178956968 years -7 mons +1 day -00:59:59.99991"),
    ),
    (
        "1 year 11 mons -00:00:00.000001",
        "1 year 2147483646 days 00:00:00.000001",
        Some("-1 years -11 mons +00:00:00.000001"),
        Some("2 years 11 mons 2147483646 days"),
        Some("11 mons -2147483646 days -00:00:00.000002"),
    ),
    (
        "178956970 years 7 mons 2147483647 days -00:00:00.999999",
        "-11 mons +2147483646 days 00:00:00.99",
        Some("-178956970 years -7 mons -2147483647 days +00:00:00.999999"),
        None,
        None,
    ),
    (
        "-178956970 years -7 mons +2 days -2562047788:00:54.775806",
        "-178956970 years -7 mons +01:00:00",
        Some("178956970 years 7 mons -2 days +2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "2 years 2147483646 days -00:00:00.999999",
        "1 mon 2147483647 days -00:00:00.000001",
        Some("-2 years -2147483646 days +00:00:00.999999"),
        None,
        Some("1 year 11 mons -1 days -00:00:00.999998"),
    ),
    (
        "-11 mons +2 days -2562047788:00:54.775806",
        "1 year 11 mons -2147483648 days +2562047788:00:54.775807",
        Some("11 mons -2 days +2562047788:00:54.775806"),
        Some("1 year -2147483646 days +00:00:00.000001"),
        None,
    ),
    (
        "1 year 1 mon -1 days +2562047788:00:54.775807",
        "2147483647 days 00:00:00.00009",
        Some("-1 years -1 mons +1 day -2562047788:00:54.775807"),
        None,
        Some("1 year 1 mon -2147483648 days +2562047788:00:54.775717"),
    ),
    (
        "-120544983 years -9 mons +407531724 days -57648153:27:01.365058",
        "1 year 1 mon 1 day",
        Some("120544983 years 9 mons -407531724 days +57648153:27:01.365058"),
        Some("-120544982 years -8 mons +407531725 days -57648153:27:01.365058"),
        Some("-120544984 years -10 mons +407531723 days -57648153:27:01.365058"),
    ),
    (
        "1 year 2147483646 days 2562047788:00:54.775806",
        "-107569204 years -562190079 days",
        Some("-1 years -2147483646 days -2562047788:00:54.775806"),
        Some("-107569203 years +1585293567 days 2562047788:00:54.775806"),
        None,
    ),
    (
        "178956970 years 6 mons 00:00:00.000001",
        "178956970 years 6 mons -2147483648 days +00:00:00.999999",
        Some("-178956970 years -6 mons -00:00:00.000001"),
        None,
        None,
    ),
    (
        "1 year 1 mon 2147483646 days 2562047788:00:54.775806",
        "-82667268 years -2 mons +323029271 days -00:00:00.000001",
        Some("-1 years -1 mons -2147483646 days -2562047788:00:54.775806"),
        None,
        Some("82667269 years 3 mons 1824454375 days 2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons 1 day -00:00:00.999999",
        "-13984591 years -2 mons -657824985 days +00:00:00.000001",
        Some("-1 years -11 mons -1 days +00:00:00.999999"),
        Some("-13984589 years -3 mons -657824984 days -00:00:00.999998"),
        Some("13984593 years 1 mon 657824986 days -00:00:01"),
    ),
    (
        "-11 mons +2147483646 days 2562047788:00:54.775806",
        "1 mon 2147483647 days 2562047788:00:54.775807",
        Some("11 mons -2147483646 days -2562047788:00:54.775806"),
        None,
        Some("-1 years -1 days -00:00:00.000001"),
    ),
    (
        "-178956970 years -8 mons +2 days 2562047788:00:54.775806",
        "-21144227 years -4 mons +884901852 days -00:00:00.000001",
        None,
        None,
        Some("-157812743 years -4 mons -884901850 days +2562047788:00:54.775807"),
    ),
    (
        "11 mons -2147483648 days -2562047788:00:54.775806",
        "-1 years -1 mons -1 days -00:00:00.000001",
        None,
        None,
        Some("2 years -2147483647 days -2562047788:00:54.775805"),
    ),
    (
        "1 year 11 mons 2147483646 days",
        "11 mons -2 days +00:00:00.00009",
        Some("-1 years -11 mons -2147483646 days"),
        Some("2 years 10 mons 2147483644 days 00:00:00.00009"),
        None,
    ),
    (
        "65498324 years 451217714 days 00:00:00.000001",
        "-178956970 years -7 mons -2 days +00:00:00.000001",
        Some("-65498324 years -451217714 days -00:00:00.000001"),
        Some("-113458646 years -7 mons +451217712 days 00:00:00.000002"),
        None,
    ),
    (
        "-11 mons +1 day 00:00:00.999999",
        "-11 mons +2147483647 days -2562047788:00:54.775806",
        Some("11 mons -1 days -00:00:00.999999"),
        None,
        None,
    ),
    (
        "-1 mons -2147483648 days -01:00:00",
        "-1 mons -1 days +00:00:00.00009",
        None,
        None,
        Some("-2147483647 days -01:00:00.00009"),
    ),
    (
        "2147483646 days 01:00:00",
        "178956970 years 7 mons 2147483646 days -2562047788:00:54.775806",
        Some("-2147483646 days -01:00:00"),
        None,
        None,
    ),
    (
        "178956970 years 6 mons -1 days +00:00:00.000001",
        "-1 years -2 days +01:00:00",
        Some("-178956970 years -6 mons +1 day -00:00:00.000001"),
        Some("178956969 years 6 mons -3 days +01:00:00.000001"),
        None,
    ),
    (
        "1 year 11 mons 2147483647 days -2562047788:00:54.775806",
        "1 year 1 mon -1 days +00:00:00.000001",
        Some("-1 years -11 mons -2147483647 days +2562047788:00:54.775806"),
        Some("3 years 2147483646 days -2562047788:00:54.775805"),
        None,
    ),
    (
        "-1 years -1 mons +2 days -2562047788:00:54.775806",
        "-1 mons +2147483646 days -2562047788:00:54.775807",
        Some("1 year 1 mon -2 days +2562047788:00:54.775806"),
        None,
        Some("-1 years -2147483644 days +00:00:00.000001"),
    ),
    (
        "1 year 11 mons 2147483646 days 01:00:00",
        "-18167939 years -5 mons +277365915 days -00:00:00.000001",
        Some("-1 years -11 mons -2147483646 days -01:00:00"),
        None,
        Some("18167941 years 4 mons 1870117731 days 01:00:00.000001"),
    ),
    (
        "-1 years -1 mons +2147483646 days -00:00:00.999999",
        "-1 years -1 days -00:00:00.999999",
        Some("1 year 1 mon -2147483646 days +00:00:00.999999"),
        Some("-2 years -1 mons +2147483645 days -00:00:01.999998"),
        Some("-1 mons +2147483647 days"),
    ),
    (
        "-178956970 years -8 mons +2 days -2562047788:00:54.775806",
        "-99038405 years -11 mons -654566775 days -00:00:00.000001",
        None,
        None,
        Some("-79918564 years -9 mons +654566777 days -2562047788:00:54.775805"),
    ),
    (
        "40614575 years 7 mons -736244039 days -59694583:07:55.544185",
        "178956970 years 7 mons 2147483647 days 00:00:00.999999",
        Some("-40614575 years -7 mons +736244039 days 59694583:07:55.544185"),
        None,
        None,
    ),
    (
        "-178956970 years -8 mons +2 days 01:00:00",
        "1 mon 1 day 00:00:00.000001",
        None,
        Some("-178956970 years -7 mons +3 days 01:00:00.000001"),
        None,
    ),
    (
        "-178956970 years -8 mons +2 days 00:00:00.99",
        "2 years 11 mons 00:00:00.999999",
        None,
        Some("-178956967 years -9 mons +2 days 00:00:01.989999"),
        None,
    ),
    (
        "1 year 00:00:00.99",
        "178956970 years 7 mons -2147483648 days +00:00:00.99",
        Some("-1 years -00:00:00.99"),
        None,
        None,
    ),
    (
        "-1 years -1 mons +2147483647 days",
        "-2 days +01:00:00",
        Some("1 year 1 mon -2147483647 days"),
        Some("-1 years -1 mons +2147483645 days 01:00:00"),
        None,
    ),
    (
        "-178956970 years -8 mons +2147483647 days 2562047788:00:54.775807",
        "-1 years -1 mons +1 day 00:00:00.99",
        None,
        None,
        Some("-178956969 years -7 mons +2147483646 days 2562047788:00:53.785807"),
    ),
    (
        "1 year 1 mon 2562047788:00:54.775807",
        "1 year -2 days",
        Some("-1 years -1 mons -2562047788:00:54.775807"),
        Some("2 years 1 mon -2 days +2562047788:00:54.775807"),
        Some("1 mon 2 days 2562047788:00:54.775807"),
    ),
    (
        "-2 years -11 mons -2147483648 days -2562047788:00:54.775806",
        "-1 mons -2562047788:00:54.775807",
        None,
        None,
        Some("-2 years -10 mons -2147483648 days +00:00:00.000001"),
    ),
    (
        "2 years -2 days -00:00:00.000001",
        "1 year",
        Some("-2 years +2 days 00:00:00.000001"),
        Some("3 years -2 days -00:00:00.000001"),
        Some("1 year -2 days -00:00:00.000001"),
    ),
    (
        "-178956970 years -7 mons -2147483648 days +00:00:00.000001",
        "28062811 years 1 mon 504038583 days",
        None,
        Some("-150894159 years -6 mons -1643445065 days +00:00:00.000001"),
        None,
    ),
    (
        "-1 days -01:00:00",
        "-23740714 years -1 mons -593085661 days",
        Some("1 day 01:00:00"),
        Some("-23740714 years -1 mons -593085662 days -01:00:00"),
        Some("23740714 years 1 mon 593085660 days -01:00:00"),
    ),
    (
        "-2 years -11 mons +2147483647 days -00:00:00.000001",
        "2 years -2 days +2562047788:00:54.775807",
        Some("2 years 11 mons -2147483647 days +00:00:00.000001"),
        Some("-11 mons +2147483645 days 2562047788:00:54.775806"),
        None,
    ),
    (
        "99324685 years 5 mons 277324774 days",
        "178956970 years 7 mons -2147483648 days -2562047788:00:54.775806",
        Some("-99324685 years -5 mons -277324774 days"),
        None,
        None,
    ),
    (
        "-2 years -11 mons +2147483646 days 00:00:00.00009",
        "52692476 years 3 mons -146780552 days -70339767:45:36.654349",
        Some("2 years 11 mons -2147483646 days -00:00:00.00009"),
        Some("52692473 years 4 mons 2000703094 days -70339767:45:36.654259"),
        None,
    ),
    (
        "-1 years -1 mons +1 day",
        "2 years 11 mons 1 day 00:00:00.00009",
        Some("1 year 1 mon -1 days"),
        Some("1 year 10 mons 2 days 00:00:00.00009"),
        Some("-4 years -00:00:00.00009"),
    ),
    (
        "11 mons 2147483647 days -01:00:00",
        "178956970 years 7 mons 2 days -01:00:00",
        Some("-11 mons -2147483647 days +01:00:00"),
        None,
        Some("-178956969 years -8 mons +2147483645 days"),
    ),
    (
        "1 mon 2 days -01:00:00",
        "-97770158 years -8 mons -233966964 days",
        Some("-1 mons -2 days +01:00:00"),
        Some("-97770158 years -7 mons -233966962 days -01:00:00"),
        Some("97770158 years 9 mons 233966966 days -01:00:00"),
    ),
    (
        "1 year 2147483647 days 00:00:00.00009",
        "1 mon -2 days +00:00:00.999999",
        Some("-1 years -2147483647 days -00:00:00.00009"),
        Some("1 year 1 mon 2147483645 days 00:00:01.000089"),
        None,
    ),
    (
        "-178956970 years -8 mons +1 day 00:00:00.00009",
        "-1 years +00:00:00.999999",
        None,
        None,
        Some("-178956969 years -8 mons +1 day -00:00:00.999909"),
    ),
    (
        "178956970 years 6 mons -00:00:00.999999",
        "-20881798 years -5 mons -69968499 days -00:00:00.552255",
        Some("-178956970 years -6 mons +00:00:00.999999"),
        Some("158075172 years 1 mon -69968499 days -00:00:01.552254"),
        None,
    ),
    (
        "-11 mons -2 days",
        "-11 mons +2147483647 days -2562047788:00:54.775806",
        Some("11 mons 2 days"),
        Some("-1 years -10 mons +2147483645 days -2562047788:00:54.775806"),
        None,
    ),
    (
        "178956970 years 6 mons -2562047788:00:54.775807",
        "-178956970 years -7 mons +2 days 2562047788:00:54.775806",
        Some("-178956970 years -6 mons +2562047788:00:54.775807"),
        Some("-1 mons +2 days -00:00:00.000001"),
        None,
    ),
    (
        "178956970 years 6 mons -2147483648 days +00:00:00.99",
        "2147483646 days 01:00:00",
        None,
        Some("178956970 years 6 mons -2 days +01:00:00.99"),
        None,
    ),
    (
        "2 years 11 mons 00:00:00.000001",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-2 years -11 mons -00:00:00.000001"),
        Some("4 years 10 mons -2 days -2562047788:00:54.775806"),
        None,
    ),
    (
        "-1 mons -2147483648 days -00:00:00.999999",
        "-178956970 years -8 mons +2 days 00:00:00.00009",
        None,
        None,
        None,
    ),
    (
        "-1 mons -2562047788:00:54.775807",
        "2 years 1 day 00:00:00.000001",
        Some("1 mon 2562047788:00:54.775807"),
        Some("1 year 11 mons 1 day -2562047788:00:54.775806"),
        Some("-2 years -1 mons -1 days -2562047788:00:54.775808"),
    ),
    (
        "89611672 years 5 mons -835398134 days +00:00:00.000001",
        "1 year 11 mons 2147483647 days 00:00:00.999999",
        Some("-89611672 years -5 mons +835398134 days -00:00:00.000001"),
        Some("89611674 years 4 mons 1312085513 days 00:00:01"),
        None,
    ),
    (
        "-178956970 years -8 mons +2147483646 days 00:00:00.000001",
        "-11 mons -2 days +00:00:00.99",
        None,
        None,
        None,
    ),
    (
        "1 mon -2562047788:00:54.775807",
        "-11 mons +2 days",
        Some("-1 mons +2562047788:00:54.775807"),
        Some("-10 mons +2 days -2562047788:00:54.775807"),
        Some("1 year -2 days -2562047788:00:54.775807"),
    ),
    (
        "11 mons -2147483648 days",
        "-1 mons +2 days 2562047788:00:54.775806",
        None,
        Some("10 mons -2147483646 days +2562047788:00:54.775806"),
        None,
    ),
    (
        "11 mons -1 days +00:00:00.99",
        "91013310 years 1 mon -731427233 days -68152696:57:14.119449",
        Some("-11 mons +1 day -00:00:00.99"),
        Some("91013311 years -731427234 days -68152696:57:13.129449"),
        Some("-91013309 years -2 mons +731427232 days 68152696:57:15.109449"),
    ),
    (
        "1 year 2 days -2562047788:00:54.775806",
        "-178956970 years -7 mons +1 day -00:00:00.000001",
        Some("-1 years -2 days +2562047788:00:54.775806"),
        Some("-178956969 years -7 mons +3 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-1 mons -2147483648 days +00:00:00.00009",
        "1 year 11 mons 1 day -2562047788:00:54.775806",
        None,
        Some("1 year 10 mons -2147483647 days -2562047788:00:54.775716"),
        None,
    ),
    (
        "-30176117 years -10 mons +839705765 days -00:00:00.000001",
        "1 mon 2147483647 days -2562047788:00:54.775807",
        Some("30176117 years 10 mons -839705765 days +00:00:00.000001"),
        None,
        Some("-30176117 years -11 mons -1307777882 days +2562047788:00:54.775806"),
    ),
    (
        "1 year 11 mons 1 day -2562047788:00:54.775807",
        "1 mon 2 days -2562047788:00:54.775806",
        Some("-1 years -11 mons -1 days +2562047788:00:54.775807"),
        None,
        Some("1 year 10 mons -1 days -00:00:00.000001"),
    ),
    (
        "1 mon 2147483646 days 00:00:00.00009",
        "-72676762 years -3 mons -372319547 days +00:00:00.000001",
        Some("-1 mons -2147483646 days -00:00:00.00009"),
        Some("-72676762 years -2 mons +1775164099 days 00:00:00.000091"),
        None,
    ),
    (
        "-1 mons -1 days",
        "49814586 years 7 mons -812332061 days -00:00:00.000001",
        Some("1 mon 1 day"),
        Some("49814586 years 6 mons -812332062 days -00:00:00.000001"),
        Some("-49814586 years -8 mons +812332060 days 00:00:00.000001"),
    ),
    (
        "59689953 years 5 mons -39752546 days -00:00:00.8045",
        "-20881798 years -5 mons -69968499 days -00:00:00.552255",
        Some("-59689953 years -5 mons +39752546 days 00:00:00.8045"),
        Some("38808155 years -109721045 days -00:00:01.356755"),
        Some("80571751 years 10 mons 30215953 days -00:00:00.252245"),
    ),
    (
        "1 mon 00:00:00.999999",
        "-1 years -1 mons -2 days -00:00:00.000001",
        Some("-1 mons -00:00:00.999999"),
        Some("-1 years -2 days +00:00:00.999998"),
        Some("1 year 2 mons 2 days 00:00:01"),
    ),
    (
        "84311177 years 8 mons -67678558 days -00:00:00.000001",
        "20063269 years -480970306 days -00:00:00.000001",
        Some("-84311177 years -8 mons +67678558 days 00:00:00.000001"),
        Some("104374446 years 8 mons -548648864 days -00:00:00.000002"),
        Some("64247908 years 8 mons 413291748 days"),
    ),
    (
        "-178956970 years -7 mons +1 day 00:00:00.00009",
        "1 year 11 mons 2562047788:00:54.775806",
        Some("178956970 years 7 mons -1 days -00:00:00.00009"),
        None,
        None,
    ),
    (
        "1 year 1 mon -2147483648 days -2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775806",
        None,
        None,
        None,
    ),
    (
        "-51683677 years +310601707 days 00:00:00.000001",
        "-1 mons +2147483646 days 2562047788:00:54.775806",
        Some("51683677 years -310601707 days -00:00:00.000001"),
        None,
        Some("-51683676 years -11 mons -1836881939 days -2562047788:00:54.775805"),
    ),
    (
        "-11 mons -2 days +00:00:00.999999",
        "-81814948 years -6 mons +625864258 days -00:00:00.435972",
        Some("11 mons 2 days -00:00:00.999999"),
        Some("-81814949 years -5 mons +625864256 days 00:00:00.564027"),
        Some("81814947 years 7 mons -625864260 days +00:00:01.435971"),
    ),
    (
        "178956970 years 7 mons 1 day",
        "-88834117 years -3 mons -876474281 days +00:00:00.000001",
        Some("-178956970 years -7 mons -1 days"),
        Some("90122853 years 4 mons -876474280 days +00:00:00.000001"),
        None,
    ),
    (
        "10369245 years 8 mons -866996521 days",
        "23595410 years 7 mons -730371216 days",
        Some("-10369245 years -8 mons +866996521 days"),
        Some("33964656 years 3 mons -1597367737 days"),
        Some("-13226164 years -11 mons -136625305 days"),
    ),
    (
        "1 mon -2 days -00:00:00.000001",
        "-178956970 years -8 mons +1 day 00:00:00.999999",
        Some("-1 mons +2 days 00:00:00.000001"),
        Some("-178956970 years -7 mons -1 days +00:00:00.999998"),
        None,
    ),
    (
        "48028885 years 11 mons -599926783 days +47511315:41:43.275879",
        "11 mons 2147483646 days 00:00:00.999999",
        Some("-48028885 years -11 mons +599926783 days -47511315:41:43.275879"),
        Some("48028886 years 10 mons 1547556863 days 47511315:41:44.275878"),
        None,
    ),
    (
        "178956970 years 7 mons 2 days 00:00:00.000001",
        "14013787 years 8 mons 655476587 days",
        Some("-178956970 years -7 mons -2 days -00:00:00.000001"),
        None,
        Some("164943182 years 11 mons -655476585 days +00:00:00.000001"),
    ),
    (
        "2 years 11 mons -1 days -2562047788:00:54.775807",
        "-178956970 years -7 mons +2147483646 days 00:00:00.99",
        Some("-2 years -11 mons +1 day 2562047788:00:54.775807"),
        Some("-178956967 years -8 mons +2147483645 days -2562047788:00:53.785807"),
        None,
    ),
    (
        "-11 mons +2 days -00:00:00.000001",
        "178956970 years 6 mons 2 days 00:00:00.00009",
        Some("11 mons -2 days +00:00:00.000001"),
        Some("178956969 years 7 mons 4 days 00:00:00.000089"),
        None,
    ),
    (
        "178956970 years 6 mons 1 day 2562047788:00:54.775807",
        "-178956970 years -7 mons +2147483647 days 00:00:00.999999",
        Some("-178956970 years -6 mons -1 days -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "95068107 years 7 mons 638721178 days 00:00:00.000001",
        "-9821949 years -5 mons -589175419 days -00:00:00.000001",
        Some("-95068107 years -7 mons -638721178 days -00:00:00.000001"),
        Some("85246158 years 2 mons 49545759 days"),
        Some("104890057 years 1227896597 days 00:00:00.000002"),
    ),
    (
        "2147483646 days",
        "-54637773 years -2 mons +575265433 days 00:00:00.000001",
        Some("-2147483646 days"),
        None,
        Some("54637773 years 2 mons 1572218213 days -00:00:00.000001"),
    ),
    (
        "-1 years -2 days -00:00:00.000001",
        "-178956970 years -7 mons -2147483648 days -01:00:00",
        Some("1 year 2 days 00:00:00.000001"),
        None,
        Some("178956969 years 7 mons 2147483646 days 00:59:59.999999"),
    ),
    (
        "-2 years -11 mons +2147483646 days 2562047788:00:54.775807",
        "-58365842 years -3 mons -697871415 days +13832733:35:04.777158",
        Some("2 years 11 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "2 years 2147483646 days -2562047788:00:54.775806",
        "11 mons 2 days -00:00:00.999999",
        Some("-2 years -2147483646 days +2562047788:00:54.775806"),
        None,
        Some("1 year 1 mon 2147483644 days -2562047788:00:53.775807"),
    ),
    (
        "-1 years +1 day 2562047788:00:54.775806",
        "-106368537 years -2 mons -133998866 days",
        Some("1 year -1 days -2562047788:00:54.775806"),
        Some("-106368538 years -2 mons -133998865 days +2562047788:00:54.775806"),
        Some("106368536 years 2 mons 133998867 days 2562047788:00:54.775806"),
    ),
    (
        "1 year 1 mon 2147483647 days 2562047788:00:54.775806",
        "-178956970 years -7 mons +2 days -00:00:00.999999",
        Some("-1 years -1 mons -2147483647 days -2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "-178956970 years -7 mons -2147483648 days",
        "1 mon 2147483647 days 00:00:00.999999",
        None,
        Some("-178956970 years -6 mons -1 days +00:00:00.999999"),
        None,
    ),
    (
        "178956970 years 6 mons 00:00:00.99",
        "-1 years -1 mons -1 days +00:00:00.999999",
        Some("-178956970 years -6 mons -00:00:00.99"),
        Some("178956969 years 5 mons -1 days +00:00:01.989999"),
        None,
    ),
    (
        "-69303989 years -1 mons -593036991 days +00:00:00.000001",
        "-1 mons +2147483647 days 01:00:00",
        Some("69303989 years 1 mon 593036991 days -00:00:00.000001"),
        Some("-69303989 years -2 mons +1554446656 days 01:00:00.000001"),
        None,
    ),
    (
        "1 year 1 day 01:00:00",
        "-1 years -1 days +00:00:00.999999",
        Some("-1 years -1 days -01:00:00"),
        Some("01:00:00.999999"),
        Some("2 years 2 days 00:59:59.000001"),
    ),
    (
        "-1 years +1 day 00:00:00.00009",
        "1 year 2 days 00:00:00.00009",
        Some("1 year -1 days -00:00:00.00009"),
        Some("3 days 00:00:00.00018"),
        Some("-2 years -1 days"),
    ),
    (
        "-1 mons +2147483646 days -2562047788:00:54.775806",
        "1 year 11 mons 2 days 2562047788:00:54.775806",
        Some("1 mon -2147483646 days +2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "11 mons 2 days 2562047788:00:54.775807",
        "98687775 years 8 mons -128658462 days -00:00:00.000001",
        Some("-11 mons -2 days -2562047788:00:54.775807"),
        Some("98687776 years 7 mons -128658460 days +2562047788:00:54.775806"),
        None,
    ),
    (
        "-88306773 years -3 mons +109123616 days -00:00:00.000001",
        "-65329284 years -1 mons -146837992 days",
        Some("88306773 years 3 mons -109123616 days +00:00:00.000001"),
        Some("-153636057 years -4 mons -37714376 days -00:00:00.000001"),
        Some("-22977489 years -2 mons +255961608 days -00:00:00.000001"),
    ),
    (
        "1 year 1 mon -2147483648 days +00:00:00.999999",
        "2 years -2 days -00:00:00.000001",
        None,
        None,
        Some("-11 mons -2147483646 days +00:00:01"),
    ),
    (
        "-1 years -1 mons +2147483646 days -01:00:00",
        "1 year 11 mons 2147483647 days -00:00:00.999999",
        Some("1 year 1 mon -2147483646 days +01:00:00"),
        None,
        Some("-3 years -1 days -00:59:59.000001"),
    ),
    (
        "-1 mons -2 days +01:00:00",
        "-61100151 years -9 mons -684523665 days",
        Some("1 mon 2 days -01:00:00"),
        Some("-61100151 years -10 mons -684523667 days +01:00:00"),
        Some("61100151 years 8 mons 684523663 days 01:00:00"),
    ),
    (
        "-178956970 years -7 mons -01:00:00",
        "1 day -00:00:00.999999",
        Some("178956970 years 7 mons 01:00:00"),
        Some("-178956970 years -7 mons +1 day -01:00:00.999999"),
        Some("-178956970 years -7 mons -1 days -00:59:59.000001"),
    ),
    (
        "-1 mons +2147483646 days 2562047788:00:54.775806",
        "-1 mons -2147483648 days +2562047788:00:54.775806",
        Some("1 mon -2147483646 days -2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775806",
        "-178956970 years -8 mons +1 day -2562047788:00:54.775806",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "54497926 years 6 mons -406021892 days +81672780:41:11.121254",
        "2 years 11 mons -2 days -01:00:00",
        Some("-54497926 years -6 mons +406021892 days -81672780:41:11.121254"),
        Some("54497929 years 5 mons -406021894 days +81672779:41:11.121254"),
        Some("54497923 years 7 mons -406021890 days +81672781:41:11.121254"),
    ),
    (
        "78069361 years -842644383 days +00:00:00.000001",
        "178956970 years 6 mons -2147483648 days +01:00:00",
        Some("-78069361 years +842644383 days -00:00:00.000001"),
        None,
        Some("-100887609 years -6 mons +1304839265 days -00:59:59.999999"),
    ),
    (
        "-11 mons +2 days -00:00:00.000001",
        "4788787 years 2 mons -833366338 days -13981230:39:04.524397",
        Some("11 mons -2 days +00:00:00.000001"),
        Some("4788786 years 3 mons -833366336 days -13981230:39:04.524398"),
        Some("-4788788 years -1 mons +833366340 days 13981230:39:04.524396"),
    ),
    (
        "2 years 1 day 01:00:00",
        "-122122167 years -8 mons -751774543 days",
        Some("-2 years -1 days -01:00:00"),
        Some("-122122165 years -8 mons -751774542 days +01:00:00"),
        Some("122122169 years 8 mons 751774544 days 01:00:00"),
    ),
    (
        "-178956970 years -7 mons -2 days +00:00:00.00009",
        "11 mons -2147483648 days -00:00:00.999999",
        Some("178956970 years 7 mons 2 days -00:00:00.00009"),
        None,
        None,
    ),
    (
        "-10739603 years -3 mons -742150760 days +00:00:00.000001",
        "1 year 1 mon -1 days +2562047788:00:54.775806",
        Some("10739603 years 3 mons 742150760 days -00:00:00.000001"),
        Some("-10739602 years -2 mons -742150761 days +2562047788:00:54.775807"),
        Some("-10739604 years -4 mons -742150759 days -2562047788:00:54.775805"),
    ),
    (
        "1 mon -2147483648 days +00:00:00.00009",
        "2 years -00:00:00.000001",
        None,
        Some("2 years 1 mon -2147483648 days +00:00:00.000089"),
        Some("-1 years -11 mons -2147483648 days +00:00:00.000091"),
    ),
    (
        "-81814948 years -6 mons +625864258 days -00:00:00.435972",
        "-178956970 years -7 mons -1 days +00:00:00.00009",
        Some("81814948 years 6 mons -625864258 days +00:00:00.435972"),
        None,
        Some("97142022 years 1 mon 625864259 days -00:00:00.436062"),
    ),
    (
        "-11 mons -2562047788:00:54.775806",
        "2 days -00:00:00.000001",
        Some("11 mons 2562047788:00:54.775806"),
        Some("-11 mons +2 days -2562047788:00:54.775807"),
        Some("-11 mons -2 days -2562047788:00:54.775805"),
    ),
    (
        "-1 mons +2147483647 days -01:00:00",
        "1 year 1 mon -1 days +01:00:00",
        Some("1 mon -2147483647 days +01:00:00"),
        Some("1 year 2147483646 days"),
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "178956970 years 7 mons 2 days 00:00:00.000001",
        Some("2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2 days -2562047788:00:54.775806"),
        Some("-178956970 years -7 mons -2 days -2562047788:00:54.775808"),
    ),
    (
        "-01:00:00",
        "-1 years -1 mons -2 days -01:00:00",
        Some("01:00:00"),
        Some("-1 years -1 mons -2 days -02:00:00"),
        Some("1 year 1 mon 2 days"),
    ),
    (
        "178956970 years 6 mons -2147483648 days +2562047788:00:54.775807",
        "1 day",
        None,
        Some("178956970 years 6 mons -2147483647 days +2562047788:00:54.775807"),
        None,
    ),
    (
        "2 years -1 days -00:00:00.999999",
        "94531332 years 10 mons 205576815 days -00:00:00.602441",
        Some("-2 years +1 day 00:00:00.999999"),
        Some("94531334 years 10 mons 205576814 days -00:00:01.60244"),
        Some("-94531330 years -10 mons -205576816 days -00:00:00.397558"),
    ),
    (
        "-29612643 years -6 mons +207965225 days -00:00:00.000001",
        "1 mon 2 days 00:00:00.00009",
        Some("29612643 years 6 mons -207965225 days +00:00:00.000001"),
        Some("-29612643 years -5 mons +207965227 days 00:00:00.000089"),
        Some("-29612643 years -7 mons +207965223 days -00:00:00.000091"),
    ),
    (
        "2 years 11 mons -2147483648 days",
        "11 mons 00:00:00.99",
        None,
        Some("3 years 10 mons -2147483648 days +00:00:00.99"),
        Some("2 years -2147483648 days -00:00:00.99"),
    ),
    (
        "-1 mons -2 days +2562047788:00:54.775807",
        "-1 years -1 mons -2562047788:00:54.775807",
        Some("1 mon 2 days -2562047788:00:54.775807"),
        Some("-1 years -2 mons -2 days"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days -01:00:00",
        "-2 years -11 mons -2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days +01:00:00"),
        None,
        None,
    ),
    (
        "-89337563 years -7 mons +470121629 days -00:00:00.000001",
        "178956970 years 6 mons 2147483647 days 00:00:00.000001",
        Some("89337563 years 7 mons -470121629 days +00:00:00.000001"),
        None,
        None,
    ),
    (
        "1 mon -1 days -01:00:00",
        "11 mons -2147483648 days +01:00:00",
        Some("-1 mons +1 day 01:00:00"),
        None,
        Some("-10 mons +2147483647 days -02:00:00"),
    ),
    (
        "2 years 11 mons 2147483646 days 2562047788:00:54.775807",
        "2 years 11 mons 2147483646 days 00:00:00.000001",
        Some("-2 years -11 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("2562047788:00:54.775806"),
    ),
    (
        "19726706 years 8 mons -315878135 days -00:00:00.000001",
        "-178956970 years -8 mons -2147483648 days -2562047788:00:54.775806",
        Some("-19726706 years -8 mons +315878135 days 00:00:00.000001"),
        None,
        None,
    ),
    (
        "-2 years -11 mons +2147483647 days 00:00:00.00009",
        "1 year 1 mon -2 days -2562047788:00:54.775806",
        Some("2 years 11 mons -2147483647 days -00:00:00.00009"),
        Some("-1 years -10 mons +2147483645 days -2562047788:00:54.775716"),
        None,
    ),
    (
        "1 year 11 mons 2 days 2562047788:00:54.775807",
        "1 year 1 mon -1 days",
        Some("-1 years -11 mons -2 days -2562047788:00:54.775807"),
        Some("3 years 1 day 2562047788:00:54.775807"),
        Some("10 mons 3 days 2562047788:00:54.775807"),
    ),
    (
        "113339416 years 748638078 days 00:00:00.000001",
        "11 mons 2147483647 days 00:00:00.999999",
        Some("-113339416 years -748638078 days -00:00:00.000001"),
        None,
        Some("113339415 years 1 mon -1398845569 days -00:00:00.999998"),
    ),
    (
        "-1 years +2 days -2562047788:00:54.775806",
        "1 day -2562047788:00:54.775806",
        Some("1 year -2 days +2562047788:00:54.775806"),
        None,
        Some("-1 years +1 day"),
    ),
    (
        "1 year 2147483646 days 00:00:00.000001",
        "1 year 1 mon 2147483647 days 2562047788:00:54.775807",
        Some("-1 years -2147483646 days -00:00:00.000001"),
        None,
        Some("-1 mons -1 days -2562047788:00:54.775806"),
    ),
    (
        "1 mon -2 days +01:00:00",
        "15844936 years 3 mons 800661521 days 00:00:00.000001",
        Some("-1 mons +2 days -01:00:00"),
        Some("15844936 years 4 mons 800661519 days 01:00:00.000001"),
        Some("-15844936 years -2 mons -800661523 days +00:59:59.999999"),
    ),
    (
        "-1 years -1 mons -2147483648 days +00:00:00.00009",
        "-2 years -11 mons +2147483647 days -01:00:00",
        None,
        Some("-4 years -1 days -00:59:59.99991"),
        None,
    ),
    (
        "1 year 2 days 00:00:00.000001",
        "11 mons 2 days 00:00:00.99",
        Some("-1 years -2 days -00:00:00.000001"),
        Some("1 year 11 mons 4 days 00:00:00.990001"),
        Some("1 mon -00:00:00.989999"),
    ),
    (
        "-1 mons -2147483648 days -00:00:00.999999",
        "-85252776 years -1 mons -397230413 days",
        None,
        None,
        Some("85252776 years -1750253235 days -00:00:00.999999"),
    ),
    (
        "2 days 00:00:00.999999",
        "51954711 years 7 mons 699503421 days -59523373:23:52.847638",
        Some("-2 days -00:00:00.999999"),
        Some("51954711 years 7 mons 699503423 days -59523373:23:51.847639"),
        Some("-51954711 years -7 mons -699503419 days +59523373:23:53.847637"),
    ),
    (
        "22510831 years 4 mons 437939775 days 00:00:00.654139",
        "-1 years -1 mons -2 days -2562047788:00:54.775806",
        Some("-22510831 years -4 mons -437939775 days -00:00:00.654139"),
        Some("22510830 years 3 mons 437939773 days -2562047788:00:54.121667"),
        None,
    ),
    (
        "-79902972 years -2 mons -764200296 days -61339193:33:48.755139",
        "-1 mons +2147483646 days 00:00:00.99",
        Some("79902972 years 2 mons 764200296 days 61339193:33:48.755139"),
        Some("-79902972 years -3 mons +1383283350 days -61339193:33:47.765139"),
        None,
    ),
    (
        "-31802316 years -9 mons -225987642 days +00:00:00.000001",
        "-46407010 years -2 mons +890738873 days -00:00:00.127516",
        Some("31802316 years 9 mons 225987642 days -00:00:00.000001"),
        Some("-78209326 years -11 mons +664751231 days -00:00:00.127515"),
        Some("14604693 years 5 mons -1116726515 days +00:00:00.127517"),
    ),
    (
        "1 mon -2 days +2562047788:00:54.775806",
        "2 years 2147483647 days 00:00:00.99",
        Some("-1 mons +2 days -2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "-178956970 years -7 mons +2147483646 days 00:00:00.000001",
        "-178956970 years -8 mons +1 day -2562047788:00:54.775806",
        Some("178956970 years 7 mons -2147483646 days -00:00:00.000001"),
        None,
        Some("1 mon 2147483645 days 2562047788:00:54.775807"),
    ),
    (
        "1 year 1 mon 2 days 01:00:00",
        "11 mons",
        Some("-1 years -1 mons -2 days -01:00:00"),
        Some("2 years 2 days 01:00:00"),
        Some("2 mons 2 days 01:00:00"),
    ),
    (
        "-1 years +2 days 2562047788:00:54.775806",
        "-178956970 years -8 mons +2 days 2562047788:00:54.775806",
        Some("1 year -2 days -2562047788:00:54.775806"),
        None,
        Some("178956969 years 8 mons"),
    ),
    (
        "11 mons 2147483646 days -01:00:00",
        "-1 years -1 mons -1 days -00:00:00.999999",
        Some("-11 mons -2147483646 days +01:00:00"),
        Some("-2 mons +2147483645 days -01:00:00.999999"),
        Some("2 years 2147483647 days -00:59:59.000001"),
    ),
    (
        "1 year 1 mon 2147483647 days 00:00:00.00009",
        "178956970 years 7 mons 2147483647 days 2562047788:00:54.775806",
        Some("-1 years -1 mons -2147483647 days -00:00:00.00009"),
        None,
        Some("-178956969 years -6 mons -2562047788:00:54.775716"),
    ),
    (
        "11 mons 2147483646 days 2562047788:00:54.775807",
        "-2 years -11 mons +00:00:00.99",
        Some("-11 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("3 years 10 mons 2147483646 days 2562047788:00:53.785807"),
    ),
    (
        "-2147483648 days -01:00:00",
        "-1 years +2147483647 days 2562047788:00:54.775806",
        None,
        Some("-1 years -1 days +2562047787:00:54.775806"),
        None,
    ),
    (
        "-11 mons +2147483646 days",
        "-178956970 years -8 mons +00:00:00.00009",
        Some("11 mons -2147483646 days"),
        None,
        Some("178956969 years 9 mons 2147483646 days -00:00:00.00009"),
    ),
    (
        "-1 years -1 days +01:00:00",
        "178956970 years 6 mons -2562047788:00:54.775806",
        Some("1 year 1 day -01:00:00"),
        Some("178956969 years 6 mons -1 days -2562047787:00:54.775806"),
        None,
    ),
    (
        "11 mons 1 day 2562047788:00:54.775806",
        "1 mon 00:00:00.999999",
        Some("-11 mons -1 days -2562047788:00:54.775806"),
        None,
        Some("10 mons 1 day 2562047788:00:53.775807"),
    ),
    (
        "2 years 11 mons 2147483647 days 00:00:00.999999",
        "-11 mons +2 days -01:00:00",
        Some("-2 years -11 mons -2147483647 days -00:00:00.999999"),
        None,
        Some("3 years 10 mons 2147483645 days 01:00:00.999999"),
    ),
    (
        "178956970 years 6 mons 2147483646 days -01:00:00",
        "1 mon -2 days +00:00:00.999999",
        Some("-178956970 years -6 mons -2147483646 days +01:00:00"),
        Some("178956970 years 7 mons 2147483644 days -00:59:59.000001"),
        None,
    ),
    (
        "1 mon 2147483647 days 2562047788:00:54.775806",
        "2 years 2147483646 days -2562047788:00:54.775806",
        Some("-1 mons -2147483647 days -2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "2 years 2 days 01:00:00",
        "1 year 11 mons 2147483647 days 00:00:00.99",
        Some("-2 years -2 days -01:00:00"),
        None,
        Some("1 mon -2147483645 days +00:59:59.01"),
    ),
    (
        "-1 years -00:00:00.000001",
        "1 mon 2147483647 days 00:00:00.00009",
        Some("1 year 00:00:00.000001"),
        Some("-11 mons +2147483647 days 00:00:00.000089"),
        Some("-1 years -1 mons -2147483647 days -00:00:00.000091"),
    ),
    (
        "2 years -1 days +2562047788:00:54.775806",
        "-26515390 years -11 mons -440265904 days -00:00:00.000001",
        Some("-2 years +1 day -2562047788:00:54.775806"),
        Some("-26515388 years -11 mons -440265905 days +2562047788:00:54.775805"),
        Some("26515392 years 11 mons 440265903 days 2562047788:00:54.775807"),
    ),
    (
        "-2 years -11 mons -2 days -01:00:00",
        "178956970 years 7 mons -2147483648 days -2562047788:00:54.775807",
        Some("2 years 11 mons 2 days 01:00:00"),
        None,
        None,
    ),
    (
        "66956303 years 6 mons -869167508 days +00:00:00.000001",
        "-10147421 years -7 mons -757182245 days +00:00:00.000001",
        Some("-66956303 years -6 mons +869167508 days -00:00:00.000001"),
        Some("56808881 years 11 mons -1626349753 days +00:00:00.000002"),
        Some("77103725 years 1 mon -111985263 days"),
    ),
    (
        "20591780 years 2 mons 283838445 days",
        "1 year -2 days -2562047788:00:54.775806",
        Some("-20591780 years -2 mons -283838445 days"),
        Some("20591781 years 2 mons 283838443 days -2562047788:00:54.775806"),
        Some("20591779 years 2 mons 283838447 days 2562047788:00:54.775806"),
    ),
    (
        "1 mon 1 day -00:00:00.000001",
        "30219255 years 1 mon 851739031 days",
        Some("-1 mons -1 days +00:00:00.000001"),
        Some("30219255 years 2 mons 851739032 days -00:00:00.000001"),
        Some("-30219255 years -851739030 days -00:00:00.000001"),
    ),
    (
        "2 years 11 mons 2147483647 days 00:00:00.00009",
        "178956970 years 7 mons 2147483647 days 00:00:00.999999",
        Some("-2 years -11 mons -2147483647 days -00:00:00.00009"),
        None,
        Some("-178956967 years -8 mons -00:00:00.999909"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 01:00:00",
        "1 year 1 mon 2 days 2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -01:00:00"),
        None,
        Some("178956969 years 5 mons 2147483645 days -2562047787:00:54.775807"),
    ),
    (
        "2 years 1 day -2562047788:00:54.775806",
        "-1 years +1 day -01:00:00",
        Some("-2 years -1 days +2562047788:00:54.775806"),
        None,
        Some("3 years -2562047787:00:54.775806"),
    ),
    (
        "1 year 1 mon 2 days -01:00:00",
        "2 years 2147483647 days 01:00:00",
        Some("-1 years -1 mons -2 days +01:00:00"),
        None,
        Some("-11 mons -2147483645 days -02:00:00"),
    ),
    (
        "-11 mons +2147483647 days -2562047788:00:54.775806",
        "1 year 1 mon 2147483646 days 2562047788:00:54.775806",
        Some("11 mons -2147483647 days +2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "-1 mons +1 day 00:00:00.999999",
        "2 years 2 days 2562047788:00:54.775807",
        Some("1 mon -1 days -00:00:00.999999"),
        None,
        Some("-2 years -1 mons -1 days -2562047788:00:53.775808"),
    ),
    (
        "-2147483648 days -01:00:00",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        None,
        None,
        Some("-1 years -11 mons -2147483646 days +2562047787:00:54.775807"),
    ),
    (
        "-1 years +2 days 01:00:00",
        "2 years 01:00:00",
        Some("1 year -2 days -01:00:00"),
        Some("1 year 2 days 02:00:00"),
        Some("-3 years +2 days"),
    ),
    (
        "11 mons 00:00:00.000001",
        "178956970 years 7 mons 1 day -2562047788:00:54.775807",
        Some("-11 mons -00:00:00.000001"),
        None,
        None,
    ),
    (
        "-178956970 years -8 mons +2 days 2562047788:00:54.775806",
        "1 year 11 mons -00:00:00.000001",
        None,
        Some("-178956968 years -9 mons +2 days 2562047788:00:54.775805"),
        None,
    ),
    (
        "178956970 years 7 mons 2 days 2562047788:00:54.775806",
        "-1 years -1 mons -2147483648 days +00:00:00.99",
        Some("-178956970 years -7 mons -2 days -2562047788:00:54.775806"),
        None,
        None,
    ),
    (
        "60475454 years 11 mons 555749712 days 00:00:00.000001",
        "2 years -00:00:00.999999",
        Some("-60475454 years -11 mons -555749712 days -00:00:00.000001"),
        Some("60475456 years 11 mons 555749712 days -00:00:00.999998"),
        Some("60475452 years 11 mons 555749712 days 00:00:01"),
    ),
    (
        "1 year 11 mons 00:00:00.00009",
        "-178956970 years -7 mons -1 days +00:00:00.99",
        Some("-1 years -11 mons -00:00:00.00009"),
        Some("-178956968 years -8 mons -1 days +00:00:00.99009"),
        None,
    ),
    (
        "-67923643 years -9 mons -184751035 days +00:00:00.000001",
        "1 day -2562047788:00:54.775806",
        Some("67923643 years 9 mons 184751035 days -00:00:00.000001"),
        Some("-67923643 years -9 mons -184751034 days -2562047788:00:54.775805"),
        Some("-67923643 years -9 mons -184751036 days +2562047788:00:54.775807"),
    ),
    (
        "64460921 years 422408380 days 00:00:00.110691",
        "-2 years -11 mons +2147483647 days 00:00:00.000001",
        Some("-64460921 years -422408380 days -00:00:00.110691"),
        None,
        Some("64460923 years 11 mons -1725075267 days +00:00:00.11069"),
    ),
    (
        "-11 mons +2147483646 days -00:00:00.000001",
        "-107569204 years -562190079 days",
        Some("11 mons -2147483646 days +00:00:00.000001"),
        Some("-107569204 years -11 mons +1585293567 days -00:00:00.000001"),
        None,
    ),
    (
        "11 mons 1 day -2562047788:00:54.775806",
        "-9722273 years -3 mons +653925133 days 00:00:00.000001",
        Some("-11 mons -1 days +2562047788:00:54.775806"),
        Some("-9722272 years -4 mons +653925134 days -2562047788:00:54.775805"),
        Some("9722274 years 2 mons -653925132 days -2562047788:00:54.775807"),
    ),
    (
        "-2147483648 days +2562047788:00:54.775807",
        "-1 years -2147483648 days -01:00:00",
        None,
        None,
        None,
    ),
    (
        "-1 years -1 mons +1 day 00:00:00.999999",
        "-178956970 years -8 mons +2 days",
        Some("1 year 1 mon -1 days -00:00:00.999999"),
        None,
        Some("178956969 years 7 mons -1 days +00:00:00.999999"),
    ),
    (
        "11 mons 1 day 2562047788:00:54.775807",
        "-2 years -11 mons +2147483646 days",
        Some("-11 mons -1 days -2562047788:00:54.775807"),
        Some("-2 years +2147483647 days 2562047788:00:54.775807"),
        Some("3 years 10 mons -2147483645 days +2562047788:00:54.775807"),
    ),
    (
        "1 year 2147483647 days 00:00:00.99",
        "-178956970 years -8 mons +2 days -00:00:00.999999",
        Some("-1 years -2147483647 days -00:00:00.99"),
        None,
        None,
    ),
    (
        "178956970 years 6 mons 2 days 00:00:00.00009",
        "11 mons 1 day 00:00:00.999999",
        Some("-178956970 years -6 mons -2 days -00:00:00.00009"),
        None,
        Some("178956969 years 7 mons 1 day -00:00:00.999909"),
    ),
    (
        "1 year 11 mons -1 days +00:00:00.000001",
        "178956970 years 6 mons 1 day 00:00:00.00009",
        Some("-1 years -11 mons +1 day -00:00:00.000001"),
        None,
        Some("-178956968 years -7 mons -2 days -00:00:00.000089"),
    ),
    (
        "-2 years -11 mons -1 days -00:00:00.999999",
        "-88834117 years -3 mons -876474281 days +00:00:00.000001",
        Some("2 years 11 mons 1 day 00:00:00.999999"),
        Some("-88834120 years -2 mons -876474282 days -00:00:00.999998"),
        Some("88834114 years 4 mons 876474280 days -00:00:01"),
    ),
    (
        "1 mon 2 days 00:00:00.999999",
        "-2 years -11 mons +1 day -00:00:00.000001",
        Some("-1 mons -2 days -00:00:00.999999"),
        Some("-2 years -10 mons +3 days 00:00:00.999998"),
        Some("3 years 1 day 00:00:01"),
    ),
    (
        "-1 years -1 mons +1 day 2562047788:00:54.775807",
        "178956970 years 6 mons -2 days -01:00:00",
        Some("1 year 1 mon -1 days -2562047788:00:54.775807"),
        Some("178956969 years 5 mons -1 days +2562047787:00:54.775807"),
        None,
    ),
    (
        "44308090 years 4 mons -854686276 days",
        "104511946 years 11 mons 618110059 days 00:00:00.000001",
        Some("-44308090 years -4 mons +854686276 days"),
        Some("148820037 years 3 mons -236576217 days +00:00:00.000001"),
        Some("-60203856 years -7 mons -1472796335 days -00:00:00.000001"),
    ),
    (
        "11 mons -2147483648 days -2562047788:00:54.775806",
        "1 year 11 mons 1 day -00:00:00.999999",
        None,
        None,
        None,
    ),
    (
        "178956970 years 7 mons -2 days +01:00:00",
        "-92826591 years -3 mons +817023838 days 00:00:00.000001",
        Some("-178956970 years -7 mons +2 days -01:00:00"),
        Some("86130379 years 4 mons 817023836 days 01:00:00.000001"),
        None,
    ),
    (
        "-1 years -1 mons -2147483648 days +00:00:00.00009",
        "-26734691 years -4 mons +791526256 days 00:00:00.378989",
        None,
        Some("-26734692 years -5 mons -1355957392 days +00:00:00.379079"),
        None,
    ),
    (
        "-178956970 years -8 mons +2 days 2562047788:00:54.775807",
        "-11 mons +2 days 00:00:00.000001",
        None,
        None,
        Some("-178956969 years -9 mons +2562047788:00:54.775806"),
    ),
    (
        "9862962 years 8 mons 281144070 days",
        "-178956970 years -7 mons +2562047788:00:54.775806",
        Some("-9862962 years -8 mons -281144070 days"),
        Some("-169094007 years -11 mons +281144070 days 2562047788:00:54.775806"),
        None,
    ),
    (
        "-178956970 years -8 mons -2 days -2562047788:00:54.775807",
        "-1 mons +1 day -00:00:00.000001",
        None,
        None,
        Some("-178956970 years -7 mons -3 days -2562047788:00:54.775806"),
    ),
    ("-2147483648 mons", "-2147483648 mons", None, None, Some("00:00:00")),
    ("-2147483648 mons", "2147483647 mons", None, Some("-1 mons"), None),
    (
        "-2147483648 mons",
        "-2147483648 days",
        None,
        Some("-178956970 years -8 mons -2147483648 days"),
        None,
    ),
    (
        "-2147483648 mons",
        "2147483647 days",
        None,
        Some("-178956970 years -8 mons +2147483647 days"),
        Some("-178956970 years -8 mons -2147483647 days"),
    ),
    (
        "-2147483648 mons",
        "2562047788:00:54.775807",
        None,
        Some("-178956970 years -8 mons +2562047788:00:54.775807"),
        Some("-178956970 years -8 mons -2562047788:00:54.775807"),
    ),
    (
        "-2147483648 mons",
        "-2562047788:00:54.775807",
        None,
        Some("-178956970 years -8 mons -2562047788:00:54.775807"),
        Some("-178956970 years -8 mons +2562047788:00:54.775807"),
    ),
    (
        "-2147483648 mons",
        "00:00:00",
        None,
        Some("-178956970 years -8 mons"),
        Some("-178956970 years -8 mons"),
    ),
    (
        "-2147483648 mons",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        None,
        Some("-2 mons +2147483647 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "-2147483648 mons",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        None,
        None,
        Some("-1 years -4 mons +2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "-2147483648 mons",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        None,
        Some("-1 mons +2147483646 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "-2147483648 mons",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        None,
        None,
        Some("2147483646 days 2562047788:00:54.775807"),
    ),
    ("-2147483648 mons", "2147483647 mons", None, Some("-1 mons"), None),
    (
        "-2147483648 mons",
        "1 day",
        None,
        Some("-178956970 years -8 mons +1 day"),
        Some("-178956970 years -8 mons -1 days"),
    ),
    ("-2147483648 mons", "1 mon", None, Some("-178956970 years -7 mons"), None),
    ("-2147483648 mons", "1 year", None, Some("-178956969 years -8 mons"), None),
    (
        "-2147483648 mons",
        "00:00:01",
        None,
        Some("-178956970 years -8 mons +00:00:01"),
        Some("-178956970 years -8 mons -00:00:01"),
    ),
    (
        "-2147483648 mons",
        "00:00:00.000001",
        None,
        Some("-178956970 years -8 mons +00:00:00.000001"),
        Some("-178956970 years -8 mons -00:00:00.000001"),
    ),
    ("-2147483648 mons", "-1 mon", None, None, Some("-178956970 years -7 mons")),
    (
        "-2147483648 mons",
        "-1 day",
        None,
        Some("-178956970 years -8 mons -1 days"),
        Some("-178956970 years -8 mons +1 day"),
    ),
    ("-2147483648 mons", "-1 year", None, None, Some("-178956969 years -8 mons")),
    (
        "-2147483648 mons",
        "-00:00:01",
        None,
        Some("-178956970 years -8 mons -00:00:01"),
        Some("-178956970 years -8 mons +00:00:01"),
    ),
    (
        "-2147483648 mons",
        "00:00:00.999999",
        None,
        Some("-178956970 years -8 mons +00:00:00.999999"),
        Some("-178956970 years -8 mons -00:00:00.999999"),
    ),
    (
        "-2147483648 mons",
        "-00:00:00.999999",
        None,
        Some("-178956970 years -8 mons -00:00:00.999999"),
        Some("-178956970 years -8 mons +00:00:00.999999"),
    ),
    (
        "-2147483648 mons",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        None,
        Some("-178956968 years -9 mons -2 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-2147483648 mons",
        "-1 years -1 mons +2562047788:00:54.775807",
        None,
        None,
        Some("-178956969 years -7 mons -2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-2147483648 mons",
        Some("-178956970 years -7 mons"),
        Some("-1 mons"),
        None,
    ),
    (
        "2147483647 mons",
        "2147483647 mons",
        Some("-178956970 years -7 mons"),
        None,
        Some("00:00:00"),
    ),
    (
        "2147483647 mons",
        "-2147483648 days",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -2147483648 days"),
        None,
    ),
    (
        "2147483647 mons",
        "2147483647 days",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 2147483647 days"),
        Some("178956970 years 7 mons -2147483647 days"),
    ),
    (
        "2147483647 mons",
        "2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 2562047788:00:54.775807"),
        Some("178956970 years 7 mons -2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "00:00:00",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons"),
        Some("178956970 years 7 mons"),
    ),
    (
        "2147483647 mons",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        None,
        Some("1 mon -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("1 year 3 mons -2147483647 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "2147483647 mons",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        None,
        Some("-2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("-1 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "2147483647 mons",
        "2147483647 mons",
        Some("-178956970 years -7 mons"),
        None,
        Some("00:00:00"),
    ),
    (
        "2147483647 mons",
        "1 day",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 1 day"),
        Some("178956970 years 7 mons -1 days"),
    ),
    (
        "2147483647 mons",
        "1 mon",
        Some("-178956970 years -7 mons"),
        None,
        Some("178956970 years 6 mons"),
    ),
    (
        "2147483647 mons",
        "1 year",
        Some("-178956970 years -7 mons"),
        None,
        Some("178956969 years 7 mons"),
    ),
    (
        "2147483647 mons",
        "00:00:01",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 00:00:01"),
        Some("178956970 years 7 mons -00:00:01"),
    ),
    (
        "2147483647 mons",
        "00:00:00.000001",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 00:00:00.000001"),
        Some("178956970 years 7 mons -00:00:00.000001"),
    ),
    (
        "2147483647 mons",
        "-1 mon",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 6 mons"),
        None,
    ),
    (
        "2147483647 mons",
        "-1 day",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -1 days"),
        Some("178956970 years 7 mons 1 day"),
    ),
    (
        "2147483647 mons",
        "-1 year",
        Some("-178956970 years -7 mons"),
        Some("178956969 years 7 mons"),
        None,
    ),
    (
        "2147483647 mons",
        "-00:00:01",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -00:00:01"),
        Some("178956970 years 7 mons 00:00:01"),
    ),
    (
        "2147483647 mons",
        "00:00:00.999999",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 00:00:00.999999"),
        Some("178956970 years 7 mons -00:00:00.999999"),
    ),
    (
        "2147483647 mons",
        "-00:00:00.999999",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -00:00:00.999999"),
        Some("178956970 years 7 mons 00:00:00.999999"),
    ),
    (
        "2147483647 mons",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        None,
        Some("178956968 years 8 mons 2 days 2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("178956969 years 6 mons 2562047788:00:54.775807"),
        None,
    ),
    (
        "-2147483648 days",
        "-2147483648 mons",
        None,
        Some("-178956970 years -8 mons -2147483648 days"),
        None,
    ),
    (
        "-2147483648 days",
        "2147483647 mons",
        None,
        Some("178956970 years 7 mons -2147483648 days"),
        Some("-178956970 years -7 mons -2147483648 days"),
    ),
    ("-2147483648 days", "-2147483648 days", None, None, Some("00:00:00")),
    ("-2147483648 days", "2147483647 days", None, Some("-1 days"), None),
    (
        "-2147483648 days",
        "2562047788:00:54.775807",
        None,
        Some("-2147483648 days +2562047788:00:54.775807"),
        Some("-2147483648 days -2562047788:00:54.775807"),
    ),
    (
        "-2147483648 days",
        "-2562047788:00:54.775807",
        None,
        Some("-2147483648 days -2562047788:00:54.775807"),
        Some("-2147483648 days +2562047788:00:54.775807"),
    ),
    ("-2147483648 days", "00:00:00", None, Some("-2147483648 days"), Some("-2147483648 days")),
    (
        "-2147483648 days",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        None,
        Some("178956970 years 6 mons -1 days +2562047788:00:54.775807"),
        None,
    ),
    (
        "-2147483648 days",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        None,
        None,
        Some("178956969 years 4 mons -1 days +2562047788:00:54.775807"),
    ),
    (
        "-2147483648 days",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        None,
        Some("178956970 years 7 mons -2 days +2562047788:00:54.775807"),
        None,
    ),
    (
        "-2147483648 days",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        None,
        None,
        None,
    ),
    (
        "-2147483648 days",
        "2147483647 mons",
        None,
        Some("178956970 years 7 mons -2147483648 days"),
        Some("-178956970 years -7 mons -2147483648 days"),
    ),
    ("-2147483648 days", "1 day", None, Some("-2147483647 days"), None),
    (
        "-2147483648 days",
        "1 mon",
        None,
        Some("1 mon -2147483648 days"),
        Some("-1 mons -2147483648 days"),
    ),
    (
        "-2147483648 days",
        "1 year",
        None,
        Some("1 year -2147483648 days"),
        Some("-1 years -2147483648 days"),
    ),
    (
        "-2147483648 days",
        "00:00:01",
        None,
        Some("-2147483648 days +00:00:01"),
        Some("-2147483648 days -00:00:01"),
    ),
    (
        "-2147483648 days",
        "00:00:00.000001",
        None,
        Some("-2147483648 days +00:00:00.000001"),
        Some("-2147483648 days -00:00:00.000001"),
    ),
    (
        "-2147483648 days",
        "-1 mon",
        None,
        Some("-1 mons -2147483648 days"),
        Some("1 mon -2147483648 days"),
    ),
    ("-2147483648 days", "-1 day", None, None, Some("-2147483647 days")),
    (
        "-2147483648 days",
        "-1 year",
        None,
        Some("-1 years -2147483648 days"),
        Some("1 year -2147483648 days"),
    ),
    (
        "-2147483648 days",
        "-00:00:01",
        None,
        Some("-2147483648 days -00:00:01"),
        Some("-2147483648 days +00:00:01"),
    ),
    (
        "-2147483648 days",
        "00:00:00.999999",
        None,
        Some("-2147483648 days +00:00:00.999999"),
        Some("-2147483648 days -00:00:00.999999"),
    ),
    (
        "-2147483648 days",
        "-00:00:00.999999",
        None,
        Some("-2147483648 days -00:00:00.999999"),
        Some("-2147483648 days +00:00:00.999999"),
    ),
    (
        "-2147483648 days",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        None,
        None,
        Some("-1 years -11 mons -2147483646 days +2562047788:00:54.775807"),
    ),
    (
        "-2147483648 days",
        "-1 years -1 mons +2562047788:00:54.775807",
        None,
        Some("-1 years -1 mons -2147483648 days +2562047788:00:54.775807"),
        Some("1 year 1 mon -2147483648 days -2562047788:00:54.775807"),
    ),
    (
        "2147483647 days",
        "-2147483648 mons",
        Some("-2147483647 days"),
        Some("-178956970 years -8 mons +2147483647 days"),
        None,
    ),
    (
        "2147483647 days",
        "2147483647 mons",
        Some("-2147483647 days"),
        Some("178956970 years 7 mons 2147483647 days"),
        Some("-178956970 years -7 mons +2147483647 days"),
    ),
    ("2147483647 days", "-2147483648 days", Some("-2147483647 days"), Some("-1 days"), None),
    ("2147483647 days", "2147483647 days", Some("-2147483647 days"), None, Some("00:00:00")),
    (
        "2147483647 days",
        "2562047788:00:54.775807",
        Some("-2147483647 days"),
        Some("2147483647 days 2562047788:00:54.775807"),
        Some("2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "2147483647 days",
        "-2562047788:00:54.775807",
        Some("-2147483647 days"),
        Some("2147483647 days -2562047788:00:54.775807"),
        Some("2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "2147483647 days",
        "00:00:00",
        Some("-2147483647 days"),
        Some("2147483647 days"),
        Some("2147483647 days"),
    ),
    (
        "2147483647 days",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-2147483647 days"),
        None,
        Some("-178956970 years -6 mons -2562047788:00:54.775807"),
    ),
    (
        "2147483647 days",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-2147483647 days"),
        Some("-178956969 years -4 mons -2562047788:00:54.775807"),
        None,
    ),
    (
        "2147483647 days",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-2147483647 days"),
        None,
        Some("-178956970 years -7 mons +1 day -2562047788:00:54.775807"),
    ),
    (
        "2147483647 days",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-2147483647 days"),
        Some("-178956970 years -8 mons +1 day -2562047788:00:54.775807"),
        None,
    ),
    (
        "2147483647 days",
        "2147483647 mons",
        Some("-2147483647 days"),
        Some("178956970 years 7 mons 2147483647 days"),
        Some("-178956970 years -7 mons +2147483647 days"),
    ),
    ("2147483647 days", "1 day", Some("-2147483647 days"), None, Some("2147483646 days")),
    (
        "2147483647 days",
        "1 mon",
        Some("-2147483647 days"),
        Some("1 mon 2147483647 days"),
        Some("-1 mons +2147483647 days"),
    ),
    (
        "2147483647 days",
        "1 year",
        Some("-2147483647 days"),
        Some("1 year 2147483647 days"),
        Some("-1 years +2147483647 days"),
    ),
    (
        "2147483647 days",
        "00:00:01",
        Some("-2147483647 days"),
        Some("2147483647 days 00:00:01"),
        Some("2147483647 days -00:00:01"),
    ),
    (
        "2147483647 days",
        "00:00:00.000001",
        Some("-2147483647 days"),
        Some("2147483647 days 00:00:00.000001"),
        Some("2147483647 days -00:00:00.000001"),
    ),
    (
        "2147483647 days",
        "-1 mon",
        Some("-2147483647 days"),
        Some("-1 mons +2147483647 days"),
        Some("1 mon 2147483647 days"),
    ),
    ("2147483647 days", "-1 day", Some("-2147483647 days"), Some("2147483646 days"), None),
    (
        "2147483647 days",
        "-1 year",
        Some("-2147483647 days"),
        Some("-1 years +2147483647 days"),
        Some("1 year 2147483647 days"),
    ),
    (
        "2147483647 days",
        "-00:00:01",
        Some("-2147483647 days"),
        Some("2147483647 days -00:00:01"),
        Some("2147483647 days 00:00:01"),
    ),
    (
        "2147483647 days",
        "00:00:00.999999",
        Some("-2147483647 days"),
        Some("2147483647 days 00:00:00.999999"),
        Some("2147483647 days -00:00:00.999999"),
    ),
    (
        "2147483647 days",
        "-00:00:00.999999",
        Some("-2147483647 days"),
        Some("2147483647 days -00:00:00.999999"),
        Some("2147483647 days 00:00:00.999999"),
    ),
    (
        "2147483647 days",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-2147483647 days"),
        Some("1 year 11 mons 2147483645 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "2147483647 days",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-2147483647 days"),
        Some("-1 years -1 mons +2147483647 days 2562047788:00:54.775807"),
        Some("1 year 1 mon 2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "-2147483648 mons",
        Some("-2562047788:00:54.775807"),
        Some("-178956970 years -8 mons +2562047788:00:54.775807"),
        None,
    ),
    (
        "2562047788:00:54.775807",
        "2147483647 mons",
        Some("-2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2562047788:00:54.775807"),
        Some("-178956970 years -7 mons +2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "-2147483648 days",
        Some("-2562047788:00:54.775807"),
        Some("-2147483648 days +2562047788:00:54.775807"),
        None,
    ),
    (
        "2562047788:00:54.775807",
        "2147483647 days",
        Some("-2562047788:00:54.775807"),
        Some("2147483647 days 2562047788:00:54.775807"),
        Some("-2147483647 days +2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "2562047788:00:54.775807",
        Some("-2562047788:00:54.775807"),
        None,
        Some("00:00:00"),
    ),
    (
        "2562047788:00:54.775807",
        "-2562047788:00:54.775807",
        Some("-2562047788:00:54.775807"),
        Some("00:00:00"),
        None,
    ),
    (
        "2562047788:00:54.775807",
        "00:00:00",
        Some("-2562047788:00:54.775807"),
        Some("2562047788:00:54.775807"),
        Some("2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-2562047788:00:54.775807"),
        None,
        Some("-178956970 years -6 mons -2147483647 days"),
    ),
    (
        "2562047788:00:54.775807",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483647 days"),
        None,
    ),
    (
        "2562047788:00:54.775807",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-2562047788:00:54.775807"),
        None,
        Some("-178956970 years -7 mons -2147483646 days"),
    ),
    (
        "2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-2562047788:00:54.775807"),
        Some("-178956970 years -8 mons -2147483646 days"),
        None,
    ),
    (
        "2562047788:00:54.775807",
        "2147483647 mons",
        Some("-2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2562047788:00:54.775807"),
        Some("-178956970 years -7 mons +2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "1 day",
        Some("-2562047788:00:54.775807"),
        Some("1 day 2562047788:00:54.775807"),
        Some("-1 days +2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "1 mon",
        Some("-2562047788:00:54.775807"),
        Some("1 mon 2562047788:00:54.775807"),
        Some("-1 mons +2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "1 year",
        Some("-2562047788:00:54.775807"),
        Some("1 year 2562047788:00:54.775807"),
        Some("-1 years +2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "00:00:01",
        Some("-2562047788:00:54.775807"),
        None,
        Some("2562047788:00:53.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "00:00:00.000001",
        Some("-2562047788:00:54.775807"),
        None,
        Some("2562047788:00:54.775806"),
    ),
    (
        "2562047788:00:54.775807",
        "-1 mon",
        Some("-2562047788:00:54.775807"),
        Some("-1 mons +2562047788:00:54.775807"),
        Some("1 mon 2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "-1 day",
        Some("-2562047788:00:54.775807"),
        Some("-1 days +2562047788:00:54.775807"),
        Some("1 day 2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "-1 year",
        Some("-2562047788:00:54.775807"),
        Some("-1 years +2562047788:00:54.775807"),
        Some("1 year 2562047788:00:54.775807"),
    ),
    (
        "2562047788:00:54.775807",
        "-00:00:01",
        Some("-2562047788:00:54.775807"),
        Some("2562047788:00:53.775807"),
        None,
    ),
    (
        "2562047788:00:54.775807",
        "00:00:00.999999",
        Some("-2562047788:00:54.775807"),
        None,
        Some("2562047788:00:53.775808"),
    ),
    (
        "2562047788:00:54.775807",
        "-00:00:00.999999",
        Some("-2562047788:00:54.775807"),
        Some("2562047788:00:53.775808"),
        None,
    ),
    (
        "2562047788:00:54.775807",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-2562047788:00:54.775807"),
        Some("1 year 11 mons -2 days"),
        None,
    ),
    (
        "2562047788:00:54.775807",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-2562047788:00:54.775807"),
        None,
        Some("1 year 1 mon"),
    ),
    (
        "-2562047788:00:54.775807",
        "-2147483648 mons",
        Some("2562047788:00:54.775807"),
        Some("-178956970 years -8 mons -2562047788:00:54.775807"),
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "2147483647 mons",
        Some("2562047788:00:54.775807"),
        Some("178956970 years 7 mons -2562047788:00:54.775807"),
        Some("-178956970 years -7 mons -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "-2147483648 days",
        Some("2562047788:00:54.775807"),
        Some("-2147483648 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "2147483647 days",
        Some("2562047788:00:54.775807"),
        Some("2147483647 days -2562047788:00:54.775807"),
        Some("-2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "2562047788:00:54.775807",
        Some("2562047788:00:54.775807"),
        Some("00:00:00"),
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "-2562047788:00:54.775807",
        Some("2562047788:00:54.775807"),
        None,
        Some("00:00:00"),
    ),
    (
        "-2562047788:00:54.775807",
        "00:00:00",
        Some("2562047788:00:54.775807"),
        Some("-2562047788:00:54.775807"),
        Some("-2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("2562047788:00:54.775807"),
        Some("178956970 years 6 mons 2147483647 days"),
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("2562047788:00:54.775807"),
        None,
        Some("178956969 years 4 mons 2147483647 days"),
    ),
    (
        "-2562047788:00:54.775807",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2147483646 days"),
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "2147483647 mons",
        Some("2562047788:00:54.775807"),
        Some("178956970 years 7 mons -2562047788:00:54.775807"),
        Some("-178956970 years -7 mons -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "1 day",
        Some("2562047788:00:54.775807"),
        Some("1 day -2562047788:00:54.775807"),
        Some("-1 days -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "1 mon",
        Some("2562047788:00:54.775807"),
        Some("1 mon -2562047788:00:54.775807"),
        Some("-1 mons -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "1 year",
        Some("2562047788:00:54.775807"),
        Some("1 year -2562047788:00:54.775807"),
        Some("-1 years -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "00:00:01",
        Some("2562047788:00:54.775807"),
        Some("-2562047788:00:53.775807"),
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "00:00:00.000001",
        Some("2562047788:00:54.775807"),
        Some("-2562047788:00:54.775806"),
        Some("-2562047788:00:54.775808"),
    ),
    (
        "-2562047788:00:54.775807",
        "-1 mon",
        Some("2562047788:00:54.775807"),
        Some("-1 mons -2562047788:00:54.775807"),
        Some("1 mon -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "-1 day",
        Some("2562047788:00:54.775807"),
        Some("-1 days -2562047788:00:54.775807"),
        Some("1 day -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "-1 year",
        Some("2562047788:00:54.775807"),
        Some("-1 years -2562047788:00:54.775807"),
        Some("1 year -2562047788:00:54.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "-00:00:01",
        Some("2562047788:00:54.775807"),
        None,
        Some("-2562047788:00:53.775807"),
    ),
    (
        "-2562047788:00:54.775807",
        "00:00:00.999999",
        Some("2562047788:00:54.775807"),
        Some("-2562047788:00:53.775808"),
        None,
    ),
    (
        "-2562047788:00:54.775807",
        "-00:00:00.999999",
        Some("2562047788:00:54.775807"),
        None,
        Some("-2562047788:00:53.775808"),
    ),
    (
        "-2562047788:00:54.775807",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("2562047788:00:54.775807"),
        None,
        Some("-1 years -11 mons +2 days"),
    ),
    (
        "-2562047788:00:54.775807",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("2562047788:00:54.775807"),
        Some("-1 years -1 mons"),
        None,
    ),
    ("00:00:00", "-2147483648 mons", Some("00:00:00"), Some("-178956970 years -8 mons"), None),
    (
        "00:00:00",
        "2147483647 mons",
        Some("00:00:00"),
        Some("178956970 years 7 mons"),
        Some("-178956970 years -7 mons"),
    ),
    ("00:00:00", "-2147483648 days", Some("00:00:00"), Some("-2147483648 days"), None),
    (
        "00:00:00",
        "2147483647 days",
        Some("00:00:00"),
        Some("2147483647 days"),
        Some("-2147483647 days"),
    ),
    (
        "00:00:00",
        "2562047788:00:54.775807",
        Some("00:00:00"),
        Some("2562047788:00:54.775807"),
        Some("-2562047788:00:54.775807"),
    ),
    (
        "00:00:00",
        "-2562047788:00:54.775807",
        Some("00:00:00"),
        Some("-2562047788:00:54.775807"),
        Some("2562047788:00:54.775807"),
    ),
    ("00:00:00", "00:00:00", Some("00:00:00"), Some("00:00:00"), Some("00:00:00")),
    (
        "00:00:00",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("00:00:00"),
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "00:00:00",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("00:00:00"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "00:00:00",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("00:00:00"),
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:54.775807"),
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "00:00:00",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("00:00:00"),
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "00:00:00",
        "2147483647 mons",
        Some("00:00:00"),
        Some("178956970 years 7 mons"),
        Some("-178956970 years -7 mons"),
    ),
    ("00:00:00", "1 day", Some("00:00:00"), Some("1 day"), Some("-1 days")),
    ("00:00:00", "1 mon", Some("00:00:00"), Some("1 mon"), Some("-1 mons")),
    ("00:00:00", "1 year", Some("00:00:00"), Some("1 year"), Some("-1 years")),
    ("00:00:00", "00:00:01", Some("00:00:00"), Some("00:00:01"), Some("-00:00:01")),
    (
        "00:00:00",
        "00:00:00.000001",
        Some("00:00:00"),
        Some("00:00:00.000001"),
        Some("-00:00:00.000001"),
    ),
    ("00:00:00", "-1 mon", Some("00:00:00"), Some("-1 mons"), Some("1 mon")),
    ("00:00:00", "-1 day", Some("00:00:00"), Some("-1 days"), Some("1 day")),
    ("00:00:00", "-1 year", Some("00:00:00"), Some("-1 years"), Some("1 year")),
    ("00:00:00", "-00:00:01", Some("00:00:00"), Some("-00:00:01"), Some("00:00:01")),
    (
        "00:00:00",
        "00:00:00.999999",
        Some("00:00:00"),
        Some("00:00:00.999999"),
        Some("-00:00:00.999999"),
    ),
    (
        "00:00:00",
        "-00:00:00.999999",
        Some("00:00:00"),
        Some("-00:00:00.999999"),
        Some("00:00:00.999999"),
    ),
    (
        "00:00:00",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("00:00:00"),
        Some("1 year 11 mons -2 days -2562047788:00:54.775807"),
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
    ),
    (
        "00:00:00",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("00:00:00"),
        Some("-1 years -1 mons +2562047788:00:54.775807"),
        Some("1 year 1 mon -2562047788:00:54.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-2147483648 mons",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-2 mons +2147483647 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "2147483647 mons",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("-1 mons +2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-2147483648 days",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956970 years 6 mons -1 days +2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "2147483647 days",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 6 mons 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 6 mons 2147483647 days"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956970 years 6 mons 2147483647 days"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "00:00:00",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:54.775807"),
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("00:00:00"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("1 year 2 mons"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("-1 mons +1 day"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-2 mons +1 day"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "2147483647 mons",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("-1 mons +2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "1 day",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 6 mons 2147483646 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "1 mon",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 5 mons 2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "1 year",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("178956969 years 6 mons 2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "00:00:01",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:53.775807"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "00:00:00.000001",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:54.775806"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-1 mon",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956970 years 5 mons 2147483647 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-1 day",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956970 years 6 mons 2147483646 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-1 year",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956969 years 6 mons 2147483647 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-00:00:01",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:53.775807"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "00:00:00.999999",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:53.775808"),
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-00:00:00.999999",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:53.775808"),
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-2147483648 mons",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        None,
        Some("1 year 4 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "2147483647 mons",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("1 year 3 mons -2147483647 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-2147483648 days",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        None,
        Some("-178956969 years -4 mons +1 day -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "2147483647 days",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2562047788:00:54.775807"),
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "2562047788:00:54.775807",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483647 days"),
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-2562047788:00:54.775807",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        None,
        Some("-178956969 years -4 mons -2147483647 days"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "00:00:00",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("1 year 2 mons"),
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        None,
        Some("00:00:00"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("1 year 3 mons -1 days"),
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        None,
        Some("1 year 4 mons -1 days"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "2147483647 mons",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("1 year 3 mons -2147483647 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "1 day",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483646 days -2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483648 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "1 mon",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -3 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-178956969 years -5 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "1 year",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956968 years -4 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-178956970 years -4 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "00:00:01",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:53.775807"),
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "00:00:00.000001",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:54.775806"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:54.775808"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-1 mon",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -5 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-178956969 years -3 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-1 day",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483648 days -2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-1 year",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956970 years -4 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-178956968 years -4 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-00:00:01",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        None,
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:53.775807"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "00:00:00.999999",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:53.775808"),
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-00:00:00.999999",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        None,
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:53.775808"),
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956970 years -5 mons -2147483647 days"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-2147483648 mons",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("-1 mons +2147483646 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "2147483647 mons",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("2147483646 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-2147483648 days",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("178956970 years 7 mons -2 days +2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "2147483647 days",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 7 mons -1 days +2562047788:00:54.775807"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 7 mons 2147483646 days"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2147483646 days"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "00:00:00",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("1 mon -1 days"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("1 year 3 mons -1 days"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("00:00:00"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("-1 mons"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "2147483647 mons",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("2147483646 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "1 day",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 7 mons 2147483645 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "1 mon",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 6 mons 2147483646 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "1 year",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("178956969 years 7 mons 2147483646 days 2562047788:00:54.775807"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "00:00:01",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:53.775807"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "00:00:00.000001",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:54.775806"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-1 mon",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("178956970 years 6 mons 2147483646 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-1 day",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2147483645 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-1 year",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("178956969 years 7 mons 2147483646 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-00:00:01",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:53.775807"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "00:00:00.999999",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:53.775808"),
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-00:00:00.999999",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:53.775808"),
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-2147483648 mons",
        None,
        None,
        Some("-2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "2147483647 mons",
        None,
        Some("-1 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-2147483648 days",
        None,
        None,
        Some("-178956970 years -8 mons +2 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "2147483647 days",
        None,
        Some("-178956970 years -8 mons +1 day -2562047788:00:54.775807"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "2562047788:00:54.775807",
        None,
        Some("-178956970 years -8 mons -2147483646 days"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-2562047788:00:54.775807",
        None,
        None,
        Some("-178956970 years -8 mons -2147483646 days"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "00:00:00",
        None,
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807"),
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        None,
        Some("-2 mons +1 day"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        None,
        None,
        Some("-1 years -4 mons +1 day"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        None,
        Some("-1 mons"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        None,
        None,
        Some("00:00:00"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "2147483647 mons",
        None,
        Some("-1 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "1 day",
        None,
        Some("-178956970 years -8 mons -2147483645 days -2562047788:00:54.775807"),
        Some("-178956970 years -8 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "1 mon",
        None,
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "1 year",
        None,
        Some("-178956969 years -8 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "00:00:01",
        None,
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:53.775807"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "00:00:00.000001",
        None,
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:54.775806"),
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:54.775808"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-1 mon",
        None,
        None,
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-1 day",
        None,
        Some("-178956970 years -8 mons -2147483647 days -2562047788:00:54.775807"),
        Some("-178956970 years -8 mons -2147483645 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-1 year",
        None,
        None,
        Some("-178956969 years -8 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-00:00:01",
        None,
        None,
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:53.775807"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "00:00:00.999999",
        None,
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:53.775808"),
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-00:00:00.999999",
        None,
        None,
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:53.775808"),
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        None,
        None,
        None,
    ),
    (
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        "-1 years -1 mons +2562047788:00:54.775807",
        None,
        None,
        None,
    ),
    (
        "2147483647 mons",
        "-2147483648 mons",
        Some("-178956970 years -7 mons"),
        Some("-1 mons"),
        None,
    ),
    (
        "2147483647 mons",
        "2147483647 mons",
        Some("-178956970 years -7 mons"),
        None,
        Some("00:00:00"),
    ),
    (
        "2147483647 mons",
        "-2147483648 days",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -2147483648 days"),
        None,
    ),
    (
        "2147483647 mons",
        "2147483647 days",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 2147483647 days"),
        Some("178956970 years 7 mons -2147483647 days"),
    ),
    (
        "2147483647 mons",
        "2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 2562047788:00:54.775807"),
        Some("178956970 years 7 mons -2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -2562047788:00:54.775807"),
        Some("178956970 years 7 mons 2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "00:00:00",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons"),
        Some("178956970 years 7 mons"),
    ),
    (
        "2147483647 mons",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        None,
        Some("1 mon -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("1 year 3 mons -2147483647 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "2147483647 mons",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        None,
        Some("-2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("-1 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "2147483647 mons",
        "2147483647 mons",
        Some("-178956970 years -7 mons"),
        None,
        Some("00:00:00"),
    ),
    (
        "2147483647 mons",
        "1 day",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 1 day"),
        Some("178956970 years 7 mons -1 days"),
    ),
    (
        "2147483647 mons",
        "1 mon",
        Some("-178956970 years -7 mons"),
        None,
        Some("178956970 years 6 mons"),
    ),
    (
        "2147483647 mons",
        "1 year",
        Some("-178956970 years -7 mons"),
        None,
        Some("178956969 years 7 mons"),
    ),
    (
        "2147483647 mons",
        "00:00:01",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 00:00:01"),
        Some("178956970 years 7 mons -00:00:01"),
    ),
    (
        "2147483647 mons",
        "00:00:00.000001",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 00:00:00.000001"),
        Some("178956970 years 7 mons -00:00:00.000001"),
    ),
    (
        "2147483647 mons",
        "-1 mon",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 6 mons"),
        None,
    ),
    (
        "2147483647 mons",
        "-1 day",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -1 days"),
        Some("178956970 years 7 mons 1 day"),
    ),
    (
        "2147483647 mons",
        "-1 year",
        Some("-178956970 years -7 mons"),
        Some("178956969 years 7 mons"),
        None,
    ),
    (
        "2147483647 mons",
        "-00:00:01",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -00:00:01"),
        Some("178956970 years 7 mons 00:00:01"),
    ),
    (
        "2147483647 mons",
        "00:00:00.999999",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons 00:00:00.999999"),
        Some("178956970 years 7 mons -00:00:00.999999"),
    ),
    (
        "2147483647 mons",
        "-00:00:00.999999",
        Some("-178956970 years -7 mons"),
        Some("178956970 years 7 mons -00:00:00.999999"),
        Some("178956970 years 7 mons 00:00:00.999999"),
    ),
    (
        "2147483647 mons",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        None,
        Some("178956968 years 8 mons 2 days 2562047788:00:54.775807"),
    ),
    (
        "2147483647 mons",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-178956970 years -7 mons"),
        Some("178956969 years 6 mons 2562047788:00:54.775807"),
        None,
    ),
    ("1 day", "-2147483648 mons", Some("-1 days"), Some("-178956970 years -8 mons +1 day"), None),
    (
        "1 day",
        "2147483647 mons",
        Some("-1 days"),
        Some("178956970 years 7 mons 1 day"),
        Some("-178956970 years -7 mons +1 day"),
    ),
    ("1 day", "-2147483648 days", Some("-1 days"), Some("-2147483647 days"), None),
    ("1 day", "2147483647 days", Some("-1 days"), None, Some("-2147483646 days")),
    (
        "1 day",
        "2562047788:00:54.775807",
        Some("-1 days"),
        Some("1 day 2562047788:00:54.775807"),
        Some("1 day -2562047788:00:54.775807"),
    ),
    (
        "1 day",
        "-2562047788:00:54.775807",
        Some("-1 days"),
        Some("1 day -2562047788:00:54.775807"),
        Some("1 day 2562047788:00:54.775807"),
    ),
    ("1 day", "00:00:00", Some("-1 days"), Some("1 day"), Some("1 day")),
    (
        "1 day",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-1 days"),
        None,
        Some("-178956970 years -6 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "1 day",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-1 days"),
        Some("-178956969 years -4 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "1 day",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-1 days"),
        None,
        Some("-178956970 years -7 mons -2147483645 days -2562047788:00:54.775807"),
    ),
    (
        "1 day",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-1 days"),
        Some("-178956970 years -8 mons -2147483645 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "1 day",
        "2147483647 mons",
        Some("-1 days"),
        Some("178956970 years 7 mons 1 day"),
        Some("-178956970 years -7 mons +1 day"),
    ),
    ("1 day", "1 day", Some("-1 days"), Some("2 days"), Some("00:00:00")),
    ("1 day", "1 mon", Some("-1 days"), Some("1 mon 1 day"), Some("-1 mons +1 day")),
    ("1 day", "1 year", Some("-1 days"), Some("1 year 1 day"), Some("-1 years +1 day")),
    ("1 day", "00:00:01", Some("-1 days"), Some("1 day 00:00:01"), Some("1 day -00:00:01")),
    (
        "1 day",
        "00:00:00.000001",
        Some("-1 days"),
        Some("1 day 00:00:00.000001"),
        Some("1 day -00:00:00.000001"),
    ),
    ("1 day", "-1 mon", Some("-1 days"), Some("-1 mons +1 day"), Some("1 mon 1 day")),
    ("1 day", "-1 day", Some("-1 days"), Some("00:00:00"), Some("2 days")),
    ("1 day", "-1 year", Some("-1 days"), Some("-1 years +1 day"), Some("1 year 1 day")),
    ("1 day", "-00:00:01", Some("-1 days"), Some("1 day -00:00:01"), Some("1 day 00:00:01")),
    (
        "1 day",
        "00:00:00.999999",
        Some("-1 days"),
        Some("1 day 00:00:00.999999"),
        Some("1 day -00:00:00.999999"),
    ),
    (
        "1 day",
        "-00:00:00.999999",
        Some("-1 days"),
        Some("1 day -00:00:00.999999"),
        Some("1 day 00:00:00.999999"),
    ),
    (
        "1 day",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-1 days"),
        Some("1 year 11 mons -1 days -2562047788:00:54.775807"),
        Some("-1 years -11 mons +3 days 2562047788:00:54.775807"),
    ),
    (
        "1 day",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-1 days"),
        Some("-1 years -1 mons +1 day 2562047788:00:54.775807"),
        Some("1 year 1 mon 1 day -2562047788:00:54.775807"),
    ),
    ("1 mon", "-2147483648 mons", Some("-1 mons"), Some("-178956970 years -7 mons"), None),
    ("1 mon", "2147483647 mons", Some("-1 mons"), None, Some("-178956970 years -6 mons")),
    ("1 mon", "-2147483648 days", Some("-1 mons"), Some("1 mon -2147483648 days"), None),
    (
        "1 mon",
        "2147483647 days",
        Some("-1 mons"),
        Some("1 mon 2147483647 days"),
        Some("1 mon -2147483647 days"),
    ),
    (
        "1 mon",
        "2562047788:00:54.775807",
        Some("-1 mons"),
        Some("1 mon 2562047788:00:54.775807"),
        Some("1 mon -2562047788:00:54.775807"),
    ),
    (
        "1 mon",
        "-2562047788:00:54.775807",
        Some("-1 mons"),
        Some("1 mon -2562047788:00:54.775807"),
        Some("1 mon 2562047788:00:54.775807"),
    ),
    ("1 mon", "00:00:00", Some("-1 mons"), Some("1 mon"), Some("1 mon")),
    (
        "1 mon",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-1 mons"),
        None,
        Some("-178956970 years -5 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "1 mon",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-1 mons"),
        Some("-178956969 years -3 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956969 years 5 mons 2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "1 mon",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-1 mons"),
        None,
        Some("-178956970 years -6 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "1 mon",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-1 mons"),
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    ("1 mon", "2147483647 mons", Some("-1 mons"), None, Some("-178956970 years -6 mons")),
    ("1 mon", "1 day", Some("-1 mons"), Some("1 mon 1 day"), Some("1 mon -1 days")),
    ("1 mon", "1 mon", Some("-1 mons"), Some("2 mons"), Some("00:00:00")),
    ("1 mon", "1 year", Some("-1 mons"), Some("1 year 1 mon"), Some("-11 mons")),
    ("1 mon", "00:00:01", Some("-1 mons"), Some("1 mon 00:00:01"), Some("1 mon -00:00:01")),
    (
        "1 mon",
        "00:00:00.000001",
        Some("-1 mons"),
        Some("1 mon 00:00:00.000001"),
        Some("1 mon -00:00:00.000001"),
    ),
    ("1 mon", "-1 mon", Some("-1 mons"), Some("00:00:00"), Some("2 mons")),
    ("1 mon", "-1 day", Some("-1 mons"), Some("1 mon -1 days"), Some("1 mon 1 day")),
    ("1 mon", "-1 year", Some("-1 mons"), Some("-11 mons"), Some("1 year 1 mon")),
    ("1 mon", "-00:00:01", Some("-1 mons"), Some("1 mon -00:00:01"), Some("1 mon 00:00:01")),
    (
        "1 mon",
        "00:00:00.999999",
        Some("-1 mons"),
        Some("1 mon 00:00:00.999999"),
        Some("1 mon -00:00:00.999999"),
    ),
    (
        "1 mon",
        "-00:00:00.999999",
        Some("-1 mons"),
        Some("1 mon -00:00:00.999999"),
        Some("1 mon 00:00:00.999999"),
    ),
    (
        "1 mon",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-1 mons"),
        Some("2 years -2 days -2562047788:00:54.775807"),
        Some("-1 years -10 mons +2 days 2562047788:00:54.775807"),
    ),
    (
        "1 mon",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-1 mons"),
        Some("-1 years +2562047788:00:54.775807"),
        Some("1 year 2 mons -2562047788:00:54.775807"),
    ),
    ("1 year", "-2147483648 mons", Some("-1 years"), Some("-178956969 years -8 mons"), None),
    ("1 year", "2147483647 mons", Some("-1 years"), None, Some("-178956969 years -7 mons")),
    ("1 year", "-2147483648 days", Some("-1 years"), Some("1 year -2147483648 days"), None),
    (
        "1 year",
        "2147483647 days",
        Some("-1 years"),
        Some("1 year 2147483647 days"),
        Some("1 year -2147483647 days"),
    ),
    (
        "1 year",
        "2562047788:00:54.775807",
        Some("-1 years"),
        Some("1 year 2562047788:00:54.775807"),
        Some("1 year -2562047788:00:54.775807"),
    ),
    (
        "1 year",
        "-2562047788:00:54.775807",
        Some("-1 years"),
        Some("1 year -2562047788:00:54.775807"),
        Some("1 year 2562047788:00:54.775807"),
    ),
    ("1 year", "00:00:00", Some("-1 years"), Some("1 year"), Some("1 year")),
    (
        "1 year",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-1 years"),
        None,
        Some("-178956969 years -6 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "1 year",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-1 years"),
        Some("-178956968 years -4 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956970 years 4 mons 2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "1 year",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-1 years"),
        None,
        Some("-178956969 years -7 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "1 year",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-1 years"),
        Some("-178956969 years -8 mons -2147483646 days -2562047788:00:54.775807"),
        None,
    ),
    ("1 year", "2147483647 mons", Some("-1 years"), None, Some("-178956969 years -7 mons")),
    ("1 year", "1 day", Some("-1 years"), Some("1 year 1 day"), Some("1 year -1 days")),
    ("1 year", "1 mon", Some("-1 years"), Some("1 year 1 mon"), Some("11 mons")),
    ("1 year", "1 year", Some("-1 years"), Some("2 years"), Some("00:00:00")),
    ("1 year", "00:00:01", Some("-1 years"), Some("1 year 00:00:01"), Some("1 year -00:00:01")),
    (
        "1 year",
        "00:00:00.000001",
        Some("-1 years"),
        Some("1 year 00:00:00.000001"),
        Some("1 year -00:00:00.000001"),
    ),
    ("1 year", "-1 mon", Some("-1 years"), Some("11 mons"), Some("1 year 1 mon")),
    ("1 year", "-1 day", Some("-1 years"), Some("1 year -1 days"), Some("1 year 1 day")),
    ("1 year", "-1 year", Some("-1 years"), Some("00:00:00"), Some("2 years")),
    ("1 year", "-00:00:01", Some("-1 years"), Some("1 year -00:00:01"), Some("1 year 00:00:01")),
    (
        "1 year",
        "00:00:00.999999",
        Some("-1 years"),
        Some("1 year 00:00:00.999999"),
        Some("1 year -00:00:00.999999"),
    ),
    (
        "1 year",
        "-00:00:00.999999",
        Some("-1 years"),
        Some("1 year -00:00:00.999999"),
        Some("1 year 00:00:00.999999"),
    ),
    (
        "1 year",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-1 years"),
        Some("2 years 11 mons -2 days -2562047788:00:54.775807"),
        Some("-11 mons +2 days 2562047788:00:54.775807"),
    ),
    (
        "1 year",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-1 years"),
        Some("-1 mons +2562047788:00:54.775807"),
        Some("2 years 1 mon -2562047788:00:54.775807"),
    ),
    (
        "00:00:01",
        "-2147483648 mons",
        Some("-00:00:01"),
        Some("-178956970 years -8 mons +00:00:01"),
        None,
    ),
    (
        "00:00:01",
        "2147483647 mons",
        Some("-00:00:01"),
        Some("178956970 years 7 mons 00:00:01"),
        Some("-178956970 years -7 mons +00:00:01"),
    ),
    ("00:00:01", "-2147483648 days", Some("-00:00:01"), Some("-2147483648 days +00:00:01"), None),
    (
        "00:00:01",
        "2147483647 days",
        Some("-00:00:01"),
        Some("2147483647 days 00:00:01"),
        Some("-2147483647 days +00:00:01"),
    ),
    (
        "00:00:01",
        "2562047788:00:54.775807",
        Some("-00:00:01"),
        None,
        Some("-2562047788:00:53.775807"),
    ),
    (
        "00:00:01",
        "-2562047788:00:54.775807",
        Some("-00:00:01"),
        Some("-2562047788:00:53.775807"),
        None,
    ),
    ("00:00:01", "00:00:00", Some("-00:00:01"), Some("00:00:01"), Some("00:00:01")),
    (
        "00:00:01",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-00:00:01"),
        None,
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:53.775807"),
    ),
    (
        "00:00:01",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-00:00:01"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:53.775807"),
        None,
    ),
    (
        "00:00:01",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-00:00:01"),
        None,
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:53.775807"),
    ),
    (
        "00:00:01",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-00:00:01"),
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:53.775807"),
        None,
    ),
    (
        "00:00:01",
        "2147483647 mons",
        Some("-00:00:01"),
        Some("178956970 years 7 mons 00:00:01"),
        Some("-178956970 years -7 mons +00:00:01"),
    ),
    ("00:00:01", "1 day", Some("-00:00:01"), Some("1 day 00:00:01"), Some("-1 days +00:00:01")),
    ("00:00:01", "1 mon", Some("-00:00:01"), Some("1 mon 00:00:01"), Some("-1 mons +00:00:01")),
    ("00:00:01", "1 year", Some("-00:00:01"), Some("1 year 00:00:01"), Some("-1 years +00:00:01")),
    ("00:00:01", "00:00:01", Some("-00:00:01"), Some("00:00:02"), Some("00:00:00")),
    (
        "00:00:01",
        "00:00:00.000001",
        Some("-00:00:01"),
        Some("00:00:01.000001"),
        Some("00:00:00.999999"),
    ),
    ("00:00:01", "-1 mon", Some("-00:00:01"), Some("-1 mons +00:00:01"), Some("1 mon 00:00:01")),
    ("00:00:01", "-1 day", Some("-00:00:01"), Some("-1 days +00:00:01"), Some("1 day 00:00:01")),
    ("00:00:01", "-1 year", Some("-00:00:01"), Some("-1 years +00:00:01"), Some("1 year 00:00:01")),
    ("00:00:01", "-00:00:01", Some("-00:00:01"), Some("00:00:00"), Some("00:00:02")),
    (
        "00:00:01",
        "00:00:00.999999",
        Some("-00:00:01"),
        Some("00:00:01.999999"),
        Some("00:00:00.000001"),
    ),
    (
        "00:00:01",
        "-00:00:00.999999",
        Some("-00:00:01"),
        Some("00:00:00.000001"),
        Some("00:00:01.999999"),
    ),
    (
        "00:00:01",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-00:00:01"),
        Some("1 year 11 mons -2 days -2562047788:00:53.775807"),
        None,
    ),
    (
        "00:00:01",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-00:00:01"),
        None,
        Some("1 year 1 mon -2562047788:00:53.775807"),
    ),
    (
        "00:00:00.000001",
        "-2147483648 mons",
        Some("-00:00:00.000001"),
        Some("-178956970 years -8 mons +00:00:00.000001"),
        None,
    ),
    (
        "00:00:00.000001",
        "2147483647 mons",
        Some("-00:00:00.000001"),
        Some("178956970 years 7 mons 00:00:00.000001"),
        Some("-178956970 years -7 mons +00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "-2147483648 days",
        Some("-00:00:00.000001"),
        Some("-2147483648 days +00:00:00.000001"),
        None,
    ),
    (
        "00:00:00.000001",
        "2147483647 days",
        Some("-00:00:00.000001"),
        Some("2147483647 days 00:00:00.000001"),
        Some("-2147483647 days +00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "2562047788:00:54.775807",
        Some("-00:00:00.000001"),
        None,
        Some("-2562047788:00:54.775806"),
    ),
    (
        "00:00:00.000001",
        "-2562047788:00:54.775807",
        Some("-00:00:00.000001"),
        Some("-2562047788:00:54.775806"),
        None,
    ),
    (
        "00:00:00.000001",
        "00:00:00",
        Some("-00:00:00.000001"),
        Some("00:00:00.000001"),
        Some("00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-00:00:00.000001"),
        None,
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:54.775806"),
    ),
    (
        "00:00:00.000001",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-00:00:00.000001"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:54.775806"),
        None,
    ),
    (
        "00:00:00.000001",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-00:00:00.000001"),
        None,
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:54.775806"),
    ),
    (
        "00:00:00.000001",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-00:00:00.000001"),
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:54.775806"),
        None,
    ),
    (
        "00:00:00.000001",
        "2147483647 mons",
        Some("-00:00:00.000001"),
        Some("178956970 years 7 mons 00:00:00.000001"),
        Some("-178956970 years -7 mons +00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "1 day",
        Some("-00:00:00.000001"),
        Some("1 day 00:00:00.000001"),
        Some("-1 days +00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "1 mon",
        Some("-00:00:00.000001"),
        Some("1 mon 00:00:00.000001"),
        Some("-1 mons +00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "1 year",
        Some("-00:00:00.000001"),
        Some("1 year 00:00:00.000001"),
        Some("-1 years +00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "00:00:01",
        Some("-00:00:00.000001"),
        Some("00:00:01.000001"),
        Some("-00:00:00.999999"),
    ),
    (
        "00:00:00.000001",
        "00:00:00.000001",
        Some("-00:00:00.000001"),
        Some("00:00:00.000002"),
        Some("00:00:00"),
    ),
    (
        "00:00:00.000001",
        "-1 mon",
        Some("-00:00:00.000001"),
        Some("-1 mons +00:00:00.000001"),
        Some("1 mon 00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "-1 day",
        Some("-00:00:00.000001"),
        Some("-1 days +00:00:00.000001"),
        Some("1 day 00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "-1 year",
        Some("-00:00:00.000001"),
        Some("-1 years +00:00:00.000001"),
        Some("1 year 00:00:00.000001"),
    ),
    (
        "00:00:00.000001",
        "-00:00:01",
        Some("-00:00:00.000001"),
        Some("-00:00:00.999999"),
        Some("00:00:01.000001"),
    ),
    (
        "00:00:00.000001",
        "00:00:00.999999",
        Some("-00:00:00.000001"),
        Some("00:00:01"),
        Some("-00:00:00.999998"),
    ),
    (
        "00:00:00.000001",
        "-00:00:00.999999",
        Some("-00:00:00.000001"),
        Some("-00:00:00.999998"),
        Some("00:00:01"),
    ),
    (
        "00:00:00.000001",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-00:00:00.000001"),
        Some("1 year 11 mons -2 days -2562047788:00:54.775806"),
        None,
    ),
    (
        "00:00:00.000001",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-00:00:00.000001"),
        None,
        Some("1 year 1 mon -2562047788:00:54.775806"),
    ),
    ("-1 mon", "-2147483648 mons", Some("1 mon"), None, Some("178956970 years 7 mons")),
    (
        "-1 mon",
        "2147483647 mons",
        Some("1 mon"),
        Some("178956970 years 6 mons"),
        Some("-178956970 years -8 mons"),
    ),
    ("-1 mon", "-2147483648 days", Some("1 mon"), Some("-1 mons -2147483648 days"), None),
    (
        "-1 mon",
        "2147483647 days",
        Some("1 mon"),
        Some("-1 mons +2147483647 days"),
        Some("-1 mons -2147483647 days"),
    ),
    (
        "-1 mon",
        "2562047788:00:54.775807",
        Some("1 mon"),
        Some("-1 mons +2562047788:00:54.775807"),
        Some("-1 mons -2562047788:00:54.775807"),
    ),
    (
        "-1 mon",
        "-2562047788:00:54.775807",
        Some("1 mon"),
        Some("-1 mons -2562047788:00:54.775807"),
        Some("-1 mons +2562047788:00:54.775807"),
    ),
    ("-1 mon", "00:00:00", Some("1 mon"), Some("-1 mons"), Some("-1 mons")),
    (
        "-1 mon",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("1 mon"),
        Some("178956970 years 5 mons 2147483647 days 2562047788:00:54.775807"),
        Some("-178956970 years -7 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-1 mon",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("1 mon"),
        Some("-178956969 years -5 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956969 years 3 mons 2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "-1 mon",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("1 mon"),
        Some("178956970 years 6 mons 2147483646 days 2562047788:00:54.775807"),
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "-1 mon",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("1 mon"),
        None,
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:54.775807"),
    ),
    (
        "-1 mon",
        "2147483647 mons",
        Some("1 mon"),
        Some("178956970 years 6 mons"),
        Some("-178956970 years -8 mons"),
    ),
    ("-1 mon", "1 day", Some("1 mon"), Some("-1 mons +1 day"), Some("-1 mons -1 days")),
    ("-1 mon", "1 mon", Some("1 mon"), Some("00:00:00"), Some("-2 mons")),
    ("-1 mon", "1 year", Some("1 mon"), Some("11 mons"), Some("-1 years -1 mons")),
    ("-1 mon", "00:00:01", Some("1 mon"), Some("-1 mons +00:00:01"), Some("-1 mons -00:00:01")),
    (
        "-1 mon",
        "00:00:00.000001",
        Some("1 mon"),
        Some("-1 mons +00:00:00.000001"),
        Some("-1 mons -00:00:00.000001"),
    ),
    ("-1 mon", "-1 mon", Some("1 mon"), Some("-2 mons"), Some("00:00:00")),
    ("-1 mon", "-1 day", Some("1 mon"), Some("-1 mons -1 days"), Some("-1 mons +1 day")),
    ("-1 mon", "-1 year", Some("1 mon"), Some("-1 years -1 mons"), Some("11 mons")),
    ("-1 mon", "-00:00:01", Some("1 mon"), Some("-1 mons -00:00:01"), Some("-1 mons +00:00:01")),
    (
        "-1 mon",
        "00:00:00.999999",
        Some("1 mon"),
        Some("-1 mons +00:00:00.999999"),
        Some("-1 mons -00:00:00.999999"),
    ),
    (
        "-1 mon",
        "-00:00:00.999999",
        Some("1 mon"),
        Some("-1 mons -00:00:00.999999"),
        Some("-1 mons +00:00:00.999999"),
    ),
    (
        "-1 mon",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("1 mon"),
        Some("1 year 10 mons -2 days -2562047788:00:54.775807"),
        Some("-2 years +2 days 2562047788:00:54.775807"),
    ),
    (
        "-1 mon",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("1 mon"),
        Some("-1 years -2 mons +2562047788:00:54.775807"),
        Some("1 year -2562047788:00:54.775807"),
    ),
    ("-1 day", "-2147483648 mons", Some("1 day"), Some("-178956970 years -8 mons -1 days"), None),
    (
        "-1 day",
        "2147483647 mons",
        Some("1 day"),
        Some("178956970 years 7 mons -1 days"),
        Some("-178956970 years -7 mons -1 days"),
    ),
    ("-1 day", "-2147483648 days", Some("1 day"), None, Some("2147483647 days")),
    ("-1 day", "2147483647 days", Some("1 day"), Some("2147483646 days"), Some("-2147483648 days")),
    (
        "-1 day",
        "2562047788:00:54.775807",
        Some("1 day"),
        Some("-1 days +2562047788:00:54.775807"),
        Some("-1 days -2562047788:00:54.775807"),
    ),
    (
        "-1 day",
        "-2562047788:00:54.775807",
        Some("1 day"),
        Some("-1 days -2562047788:00:54.775807"),
        Some("-1 days +2562047788:00:54.775807"),
    ),
    ("-1 day", "00:00:00", Some("1 day"), Some("-1 days"), Some("-1 days")),
    (
        "-1 day",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("1 day"),
        Some("178956970 years 6 mons 2147483646 days 2562047788:00:54.775807"),
        Some("-178956970 years -6 mons -2147483648 days -2562047788:00:54.775807"),
    ),
    (
        "-1 day",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("1 day"),
        Some("-178956969 years -4 mons -2147483648 days -2562047788:00:54.775807"),
        Some("178956969 years 4 mons 2147483646 days 2562047788:00:54.775807"),
    ),
    (
        "-1 day",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("1 day"),
        Some("178956970 years 7 mons 2147483645 days 2562047788:00:54.775807"),
        Some("-178956970 years -7 mons -2147483647 days -2562047788:00:54.775807"),
    ),
    (
        "-1 day",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("1 day"),
        Some("-178956970 years -8 mons -2147483647 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "-1 day",
        "2147483647 mons",
        Some("1 day"),
        Some("178956970 years 7 mons -1 days"),
        Some("-178956970 years -7 mons -1 days"),
    ),
    ("-1 day", "1 day", Some("1 day"), Some("00:00:00"), Some("-2 days")),
    ("-1 day", "1 mon", Some("1 day"), Some("1 mon -1 days"), Some("-1 mons -1 days")),
    ("-1 day", "1 year", Some("1 day"), Some("1 year -1 days"), Some("-1 years -1 days")),
    ("-1 day", "00:00:01", Some("1 day"), Some("-1 days +00:00:01"), Some("-1 days -00:00:01")),
    (
        "-1 day",
        "00:00:00.000001",
        Some("1 day"),
        Some("-1 days +00:00:00.000001"),
        Some("-1 days -00:00:00.000001"),
    ),
    ("-1 day", "-1 mon", Some("1 day"), Some("-1 mons -1 days"), Some("1 mon -1 days")),
    ("-1 day", "-1 day", Some("1 day"), Some("-2 days"), Some("00:00:00")),
    ("-1 day", "-1 year", Some("1 day"), Some("-1 years -1 days"), Some("1 year -1 days")),
    ("-1 day", "-00:00:01", Some("1 day"), Some("-1 days -00:00:01"), Some("-1 days +00:00:01")),
    (
        "-1 day",
        "00:00:00.999999",
        Some("1 day"),
        Some("-1 days +00:00:00.999999"),
        Some("-1 days -00:00:00.999999"),
    ),
    (
        "-1 day",
        "-00:00:00.999999",
        Some("1 day"),
        Some("-1 days -00:00:00.999999"),
        Some("-1 days +00:00:00.999999"),
    ),
    (
        "-1 day",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("1 day"),
        Some("1 year 11 mons -3 days -2562047788:00:54.775807"),
        Some("-1 years -11 mons +1 day 2562047788:00:54.775807"),
    ),
    (
        "-1 day",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("1 day"),
        Some("-1 years -1 mons -1 days +2562047788:00:54.775807"),
        Some("1 year 1 mon -1 days -2562047788:00:54.775807"),
    ),
    ("-1 year", "-2147483648 mons", Some("1 year"), None, Some("178956969 years 8 mons")),
    ("-1 year", "2147483647 mons", Some("1 year"), Some("178956969 years 7 mons"), None),
    ("-1 year", "-2147483648 days", Some("1 year"), Some("-1 years -2147483648 days"), None),
    (
        "-1 year",
        "2147483647 days",
        Some("1 year"),
        Some("-1 years +2147483647 days"),
        Some("-1 years -2147483647 days"),
    ),
    (
        "-1 year",
        "2562047788:00:54.775807",
        Some("1 year"),
        Some("-1 years +2562047788:00:54.775807"),
        Some("-1 years -2562047788:00:54.775807"),
    ),
    (
        "-1 year",
        "-2562047788:00:54.775807",
        Some("1 year"),
        Some("-1 years -2562047788:00:54.775807"),
        Some("-1 years +2562047788:00:54.775807"),
    ),
    ("-1 year", "00:00:00", Some("1 year"), Some("-1 years"), Some("-1 years")),
    (
        "-1 year",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("1 year"),
        Some("178956969 years 6 mons 2147483647 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "-1 year",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("1 year"),
        Some("-178956970 years -4 mons -2147483647 days -2562047788:00:54.775807"),
        Some("178956968 years 4 mons 2147483647 days 2562047788:00:54.775807"),
    ),
    (
        "-1 year",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("1 year"),
        Some("178956969 years 7 mons 2147483646 days 2562047788:00:54.775807"),
        None,
    ),
    (
        "-1 year",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("1 year"),
        None,
        Some("178956969 years 8 mons 2147483646 days 2562047788:00:54.775807"),
    ),
    ("-1 year", "2147483647 mons", Some("1 year"), Some("178956969 years 7 mons"), None),
    ("-1 year", "1 day", Some("1 year"), Some("-1 years +1 day"), Some("-1 years -1 days")),
    ("-1 year", "1 mon", Some("1 year"), Some("-11 mons"), Some("-1 years -1 mons")),
    ("-1 year", "1 year", Some("1 year"), Some("00:00:00"), Some("-2 years")),
    ("-1 year", "00:00:01", Some("1 year"), Some("-1 years +00:00:01"), Some("-1 years -00:00:01")),
    (
        "-1 year",
        "00:00:00.000001",
        Some("1 year"),
        Some("-1 years +00:00:00.000001"),
        Some("-1 years -00:00:00.000001"),
    ),
    ("-1 year", "-1 mon", Some("1 year"), Some("-1 years -1 mons"), Some("-11 mons")),
    ("-1 year", "-1 day", Some("1 year"), Some("-1 years -1 days"), Some("-1 years +1 day")),
    ("-1 year", "-1 year", Some("1 year"), Some("-2 years"), Some("00:00:00")),
    (
        "-1 year",
        "-00:00:01",
        Some("1 year"),
        Some("-1 years -00:00:01"),
        Some("-1 years +00:00:01"),
    ),
    (
        "-1 year",
        "00:00:00.999999",
        Some("1 year"),
        Some("-1 years +00:00:00.999999"),
        Some("-1 years -00:00:00.999999"),
    ),
    (
        "-1 year",
        "-00:00:00.999999",
        Some("1 year"),
        Some("-1 years -00:00:00.999999"),
        Some("-1 years +00:00:00.999999"),
    ),
    (
        "-1 year",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("1 year"),
        Some("11 mons -2 days -2562047788:00:54.775807"),
        Some("-2 years -11 mons +2 days 2562047788:00:54.775807"),
    ),
    (
        "-1 year",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("1 year"),
        Some("-2 years -1 mons +2562047788:00:54.775807"),
        Some("1 mon -2562047788:00:54.775807"),
    ),
    (
        "-00:00:01",
        "-2147483648 mons",
        Some("00:00:01"),
        Some("-178956970 years -8 mons -00:00:01"),
        None,
    ),
    (
        "-00:00:01",
        "2147483647 mons",
        Some("00:00:01"),
        Some("178956970 years 7 mons -00:00:01"),
        Some("-178956970 years -7 mons -00:00:01"),
    ),
    ("-00:00:01", "-2147483648 days", Some("00:00:01"), Some("-2147483648 days -00:00:01"), None),
    (
        "-00:00:01",
        "2147483647 days",
        Some("00:00:01"),
        Some("2147483647 days -00:00:01"),
        Some("-2147483647 days -00:00:01"),
    ),
    (
        "-00:00:01",
        "2562047788:00:54.775807",
        Some("00:00:01"),
        Some("2562047788:00:53.775807"),
        None,
    ),
    (
        "-00:00:01",
        "-2562047788:00:54.775807",
        Some("00:00:01"),
        None,
        Some("2562047788:00:53.775807"),
    ),
    ("-00:00:01", "00:00:00", Some("00:00:01"), Some("-00:00:01"), Some("-00:00:01")),
    (
        "-00:00:01",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("00:00:01"),
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:53.775807"),
        None,
    ),
    (
        "-00:00:01",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("00:00:01"),
        None,
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:53.775807"),
    ),
    (
        "-00:00:01",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("00:00:01"),
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:53.775807"),
        None,
    ),
    (
        "-00:00:01",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("00:00:01"),
        None,
        None,
    ),
    (
        "-00:00:01",
        "2147483647 mons",
        Some("00:00:01"),
        Some("178956970 years 7 mons -00:00:01"),
        Some("-178956970 years -7 mons -00:00:01"),
    ),
    ("-00:00:01", "1 day", Some("00:00:01"), Some("1 day -00:00:01"), Some("-1 days -00:00:01")),
    ("-00:00:01", "1 mon", Some("00:00:01"), Some("1 mon -00:00:01"), Some("-1 mons -00:00:01")),
    ("-00:00:01", "1 year", Some("00:00:01"), Some("1 year -00:00:01"), Some("-1 years -00:00:01")),
    ("-00:00:01", "00:00:01", Some("00:00:01"), Some("00:00:00"), Some("-00:00:02")),
    (
        "-00:00:01",
        "00:00:00.000001",
        Some("00:00:01"),
        Some("-00:00:00.999999"),
        Some("-00:00:01.000001"),
    ),
    ("-00:00:01", "-1 mon", Some("00:00:01"), Some("-1 mons -00:00:01"), Some("1 mon -00:00:01")),
    ("-00:00:01", "-1 day", Some("00:00:01"), Some("-1 days -00:00:01"), Some("1 day -00:00:01")),
    (
        "-00:00:01",
        "-1 year",
        Some("00:00:01"),
        Some("-1 years -00:00:01"),
        Some("1 year -00:00:01"),
    ),
    ("-00:00:01", "-00:00:01", Some("00:00:01"), Some("-00:00:02"), Some("00:00:00")),
    (
        "-00:00:01",
        "00:00:00.999999",
        Some("00:00:01"),
        Some("-00:00:00.000001"),
        Some("-00:00:01.999999"),
    ),
    (
        "-00:00:01",
        "-00:00:00.999999",
        Some("00:00:01"),
        Some("-00:00:01.999999"),
        Some("-00:00:00.000001"),
    ),
    (
        "-00:00:01",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("00:00:01"),
        None,
        Some("-1 years -11 mons +2 days 2562047788:00:53.775807"),
    ),
    (
        "-00:00:01",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("00:00:01"),
        Some("-1 years -1 mons +2562047788:00:53.775807"),
        None,
    ),
    (
        "00:00:00.999999",
        "-2147483648 mons",
        Some("-00:00:00.999999"),
        Some("-178956970 years -8 mons +00:00:00.999999"),
        None,
    ),
    (
        "00:00:00.999999",
        "2147483647 mons",
        Some("-00:00:00.999999"),
        Some("178956970 years 7 mons 00:00:00.999999"),
        Some("-178956970 years -7 mons +00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "-2147483648 days",
        Some("-00:00:00.999999"),
        Some("-2147483648 days +00:00:00.999999"),
        None,
    ),
    (
        "00:00:00.999999",
        "2147483647 days",
        Some("-00:00:00.999999"),
        Some("2147483647 days 00:00:00.999999"),
        Some("-2147483647 days +00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "2562047788:00:54.775807",
        Some("-00:00:00.999999"),
        None,
        Some("-2562047788:00:53.775808"),
    ),
    (
        "00:00:00.999999",
        "-2562047788:00:54.775807",
        Some("-00:00:00.999999"),
        Some("-2562047788:00:53.775808"),
        None,
    ),
    (
        "00:00:00.999999",
        "00:00:00",
        Some("-00:00:00.999999"),
        Some("00:00:00.999999"),
        Some("00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-00:00:00.999999"),
        None,
        Some("-178956970 years -6 mons -2147483647 days -2562047788:00:53.775808"),
    ),
    (
        "00:00:00.999999",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-00:00:00.999999"),
        Some("-178956969 years -4 mons -2147483647 days -2562047788:00:53.775808"),
        None,
    ),
    (
        "00:00:00.999999",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-00:00:00.999999"),
        None,
        Some("-178956970 years -7 mons -2147483646 days -2562047788:00:53.775808"),
    ),
    (
        "00:00:00.999999",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-00:00:00.999999"),
        Some("-178956970 years -8 mons -2147483646 days -2562047788:00:53.775808"),
        None,
    ),
    (
        "00:00:00.999999",
        "2147483647 mons",
        Some("-00:00:00.999999"),
        Some("178956970 years 7 mons 00:00:00.999999"),
        Some("-178956970 years -7 mons +00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "1 day",
        Some("-00:00:00.999999"),
        Some("1 day 00:00:00.999999"),
        Some("-1 days +00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "1 mon",
        Some("-00:00:00.999999"),
        Some("1 mon 00:00:00.999999"),
        Some("-1 mons +00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "1 year",
        Some("-00:00:00.999999"),
        Some("1 year 00:00:00.999999"),
        Some("-1 years +00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "00:00:01",
        Some("-00:00:00.999999"),
        Some("00:00:01.999999"),
        Some("-00:00:00.000001"),
    ),
    (
        "00:00:00.999999",
        "00:00:00.000001",
        Some("-00:00:00.999999"),
        Some("00:00:01"),
        Some("00:00:00.999998"),
    ),
    (
        "00:00:00.999999",
        "-1 mon",
        Some("-00:00:00.999999"),
        Some("-1 mons +00:00:00.999999"),
        Some("1 mon 00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "-1 day",
        Some("-00:00:00.999999"),
        Some("-1 days +00:00:00.999999"),
        Some("1 day 00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "-1 year",
        Some("-00:00:00.999999"),
        Some("-1 years +00:00:00.999999"),
        Some("1 year 00:00:00.999999"),
    ),
    (
        "00:00:00.999999",
        "-00:00:01",
        Some("-00:00:00.999999"),
        Some("-00:00:00.000001"),
        Some("00:00:01.999999"),
    ),
    (
        "00:00:00.999999",
        "00:00:00.999999",
        Some("-00:00:00.999999"),
        Some("00:00:01.999998"),
        Some("00:00:00"),
    ),
    (
        "00:00:00.999999",
        "-00:00:00.999999",
        Some("-00:00:00.999999"),
        Some("00:00:00"),
        Some("00:00:01.999998"),
    ),
    (
        "00:00:00.999999",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-00:00:00.999999"),
        Some("1 year 11 mons -2 days -2562047788:00:53.775808"),
        None,
    ),
    (
        "00:00:00.999999",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-00:00:00.999999"),
        None,
        Some("1 year 1 mon -2562047788:00:53.775808"),
    ),
    (
        "-00:00:00.999999",
        "-2147483648 mons",
        Some("00:00:00.999999"),
        Some("-178956970 years -8 mons -00:00:00.999999"),
        None,
    ),
    (
        "-00:00:00.999999",
        "2147483647 mons",
        Some("00:00:00.999999"),
        Some("178956970 years 7 mons -00:00:00.999999"),
        Some("-178956970 years -7 mons -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "-2147483648 days",
        Some("00:00:00.999999"),
        Some("-2147483648 days -00:00:00.999999"),
        None,
    ),
    (
        "-00:00:00.999999",
        "2147483647 days",
        Some("00:00:00.999999"),
        Some("2147483647 days -00:00:00.999999"),
        Some("-2147483647 days -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "2562047788:00:54.775807",
        Some("00:00:00.999999"),
        Some("2562047788:00:53.775808"),
        None,
    ),
    (
        "-00:00:00.999999",
        "-2562047788:00:54.775807",
        Some("00:00:00.999999"),
        None,
        Some("2562047788:00:53.775808"),
    ),
    (
        "-00:00:00.999999",
        "00:00:00",
        Some("00:00:00.999999"),
        Some("-00:00:00.999999"),
        Some("-00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("00:00:00.999999"),
        Some("178956970 years 6 mons 2147483647 days 2562047788:00:53.775808"),
        None,
    ),
    (
        "-00:00:00.999999",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("00:00:00.999999"),
        None,
        Some("178956969 years 4 mons 2147483647 days 2562047788:00:53.775808"),
    ),
    (
        "-00:00:00.999999",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("00:00:00.999999"),
        Some("178956970 years 7 mons 2147483646 days 2562047788:00:53.775808"),
        None,
    ),
    (
        "-00:00:00.999999",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("00:00:00.999999"),
        None,
        None,
    ),
    (
        "-00:00:00.999999",
        "2147483647 mons",
        Some("00:00:00.999999"),
        Some("178956970 years 7 mons -00:00:00.999999"),
        Some("-178956970 years -7 mons -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "1 day",
        Some("00:00:00.999999"),
        Some("1 day -00:00:00.999999"),
        Some("-1 days -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "1 mon",
        Some("00:00:00.999999"),
        Some("1 mon -00:00:00.999999"),
        Some("-1 mons -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "1 year",
        Some("00:00:00.999999"),
        Some("1 year -00:00:00.999999"),
        Some("-1 years -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "00:00:01",
        Some("00:00:00.999999"),
        Some("00:00:00.000001"),
        Some("-00:00:01.999999"),
    ),
    (
        "-00:00:00.999999",
        "00:00:00.000001",
        Some("00:00:00.999999"),
        Some("-00:00:00.999998"),
        Some("-00:00:01"),
    ),
    (
        "-00:00:00.999999",
        "-1 mon",
        Some("00:00:00.999999"),
        Some("-1 mons -00:00:00.999999"),
        Some("1 mon -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "-1 day",
        Some("00:00:00.999999"),
        Some("-1 days -00:00:00.999999"),
        Some("1 day -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "-1 year",
        Some("00:00:00.999999"),
        Some("-1 years -00:00:00.999999"),
        Some("1 year -00:00:00.999999"),
    ),
    (
        "-00:00:00.999999",
        "-00:00:01",
        Some("00:00:00.999999"),
        Some("-00:00:01.999999"),
        Some("00:00:00.000001"),
    ),
    (
        "-00:00:00.999999",
        "00:00:00.999999",
        Some("00:00:00.999999"),
        Some("00:00:00"),
        Some("-00:00:01.999998"),
    ),
    (
        "-00:00:00.999999",
        "-00:00:00.999999",
        Some("00:00:00.999999"),
        Some("-00:00:01.999998"),
        Some("00:00:00"),
    ),
    (
        "-00:00:00.999999",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("00:00:00.999999"),
        None,
        Some("-1 years -11 mons +2 days 2562047788:00:53.775808"),
    ),
    (
        "-00:00:00.999999",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("00:00:00.999999"),
        Some("-1 years -1 mons +2562047788:00:53.775808"),
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-2147483648 mons",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("-178956968 years -9 mons -2 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "2147483647 mons",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        Some("-178956968 years -8 mons -2 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-2147483648 days",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        Some("1 year 11 mons 2147483646 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "2147483647 days",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 11 mons 2147483645 days -2562047788:00:54.775807"),
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "2562047788:00:54.775807",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 11 mons -2 days"),
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-2562047788:00:54.775807",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        Some("1 year 11 mons -2 days"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "00:00:00",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 11 mons -2 days -2562047788:00:54.775807"),
        Some("1 year 11 mons -2 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "2147483647 mons",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        Some("-178956968 years -8 mons -2 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "1 day",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 11 mons -1 days -2562047788:00:54.775807"),
        Some("1 year 11 mons -3 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "1 mon",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("2 years -2 days -2562047788:00:54.775807"),
        Some("1 year 10 mons -2 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "1 year",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("2 years 11 mons -2 days -2562047788:00:54.775807"),
        Some("11 mons -2 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "00:00:01",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 11 mons -2 days -2562047788:00:53.775807"),
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "00:00:00.000001",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 11 mons -2 days -2562047788:00:54.775806"),
        Some("1 year 11 mons -2 days -2562047788:00:54.775808"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-1 mon",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 10 mons -2 days -2562047788:00:54.775807"),
        Some("2 years -2 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-1 day",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 11 mons -3 days -2562047788:00:54.775807"),
        Some("1 year 11 mons -1 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-1 year",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("11 mons -2 days -2562047788:00:54.775807"),
        Some("2 years 11 mons -2 days -2562047788:00:54.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-00:00:01",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        Some("1 year 11 mons -2 days -2562047788:00:53.775807"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "00:00:00.999999",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("1 year 11 mons -2 days -2562047788:00:53.775808"),
        None,
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-00:00:00.999999",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        Some("1 year 11 mons -2 days -2562047788:00:53.775808"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        None,
        Some("00:00:00"),
    ),
    (
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("-1 years -11 mons +2 days 2562047788:00:54.775807"),
        Some("10 mons -2 days"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-2147483648 mons",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        Some("178956969 years 7 mons 2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "2147483647 mons",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("178956969 years 6 mons 2562047788:00:54.775807"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-2147483648 days",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -1 mons -2147483648 days +2562047788:00:54.775807"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "2147483647 days",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -1 mons +2147483647 days 2562047788:00:54.775807"),
        Some("-1 years -1 mons -2147483647 days +2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "2562047788:00:54.775807",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        Some("-1 years -1 mons"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-2562047788:00:54.775807",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -1 mons"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "00:00:00",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -1 mons +2562047788:00:54.775807"),
        Some("-1 years -1 mons +2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "178956970 years 6 mons 2147483647 days 2562047788:00:54.775807",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-178956970 years +8 mons -2147483647 days -2562047788:00:54.775807",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-178956970 years -5 mons -2147483647 days"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "178956970 years 7 mons 2147483646 days 2562047788:00:54.775807",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-178956970 years -8 mons -2147483646 days -2562047788:00:54.775807",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "2147483647 mons",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("178956969 years 6 mons 2562047788:00:54.775807"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "1 day",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -1 mons +1 day 2562047788:00:54.775807"),
        Some("-1 years -1 mons -1 days +2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "1 mon",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years +2562047788:00:54.775807"),
        Some("-1 years -2 mons +2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "1 year",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 mons +2562047788:00:54.775807"),
        Some("-2 years -1 mons +2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "00:00:01",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        Some("-1 years -1 mons +2562047788:00:53.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "00:00:00.000001",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        Some("-1 years -1 mons +2562047788:00:54.775806"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-1 mon",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -2 mons +2562047788:00:54.775807"),
        Some("-1 years +2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-1 day",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -1 mons -1 days +2562047788:00:54.775807"),
        Some("-1 years -1 mons +1 day 2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-1 year",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-2 years -1 mons +2562047788:00:54.775807"),
        Some("-1 mons +2562047788:00:54.775807"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-00:00:01",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -1 mons +2562047788:00:53.775807"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "00:00:00.999999",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        Some("-1 years -1 mons +2562047788:00:53.775808"),
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-00:00:00.999999",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("-1 years -1 mons +2562047788:00:53.775808"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "1 year 11 mons -2 days -2562047788:00:54.775807",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        Some("10 mons -2 days"),
        None,
    ),
    (
        "-1 years -1 mons +2562047788:00:54.775807",
        "-1 years -1 mons +2562047788:00:54.775807",
        Some("1 year 1 mon -2562047788:00:54.775807"),
        None,
        Some("00:00:00"),
    ),
];

/// The corpus a `sum` reads, canonical interval texts.
const SUM_ROWS: &[&str] = &[
    "-325 years +846 days 00:00:00.00009",
    "-237 years -3 mons +2602 days 00:00:00.000001",
    "315 years 4 mons -2079 days -00:00:00.000001",
    "152 years 4 mons -1857 days -00:00:00.690878",
    "118 years 8 mons -2504 days -00:00:00.000001",
    "32 years 4 mons 2999 days 00:00:00.00009",
    "-193 years -6 mons +1858 days",
    "-319 years -6 mons +300 days -00:00:00.000001",
    "-192 years -10 mons -295 days -00:00:00.999999",
    "74 years 1 mon -981 days -00:00:00.000001",
    "-49 years -2 mons +1791 days 00:00:00.389832",
    "317 years 9 mons -2578 days +00:00:00.000001",
    "-90 years -8 mons -1897 days +00:00:00.000001",
    "237 years 7 mons 83 days",
    "168 years 5 mons 1351 days 00:00:00.99",
    "111 years 2 mons -1212 days +00:00:00.00009",
    "164 years 10 mons 10 days -00:00:00.999999",
    "104 years 2 mons -1205 days +00:00:00.011024",
    "-153 years -7 mons -248 days -00:00:00.999999",
    "57 years 245 days -00:00:00.999999",
    "322 years 11 mons -1254 days",
    "205 years 10 mons -491 days +00:00:00.000001",
    "48 years 10 mons 2109 days 00:00:00.999999",
    "-269 years -6 mons -1579 days -00:00:00.595688",
    "295 years 4 mons 1926 days",
    "123 years 9 mons 270 days 00:00:00.999999",
    "109 years 11 mons -1388 days +00:00:00.99",
    "-195 years -1 mons -265 days +00:00:00.00009",
    "205 years 2 mons -2603 days",
    "-150 years -8 mons -2191 days",
    "322 years 6 mons -1410 days -00:00:00.999999",
    "-222 years -7 mons +2844 days 00:00:00.000001",
    "-8 years -8 mons -2526 days +00:00:00.999999",
    "254 years 50 days 00:00:00.99",
    "100 years 11 mons 1838 days",
    "-2 years -1871 days +00:00:00.99",
    "331 years -2799 days +00:00:00.99",
    "189 years 2 mons 849 days -00:00:00.000001",
    "-325 years -9 mons -2208 days +00:00:00.999999",
    "-29 years -1558 days +00:00:00.000001",
    "-248 years -2 mons +1646 days 00:00:00.814435",
    "297 years 2029 days -00:00:00.000001",
    "205 years 2 mons -1417 days +00:00:00.999999",
    "137 years 2 mons -1495 days +00:00:00.99",
    "47 years 11 mons 2692 days 00:00:00.00009",
    "-178 years -5 mons +1292 days 00:00:00.99",
    "-122 years -9 mons +2873 days 00:00:00.99",
    "-110 years -10 mons -1906 days",
    "23 years 5 mons -159 days -00:00:00.000001",
    "310 years 3 mons -603 days +00:00:00.00009",
    "62 years 5 mons -1639 days",
    "166 years 9 mons 480 days 00:00:00.000001",
    "-165 years -9 mons +118 days 00:00:00.999999",
    "-193 years -7 mons +485 days 00:00:00.999999",
    "36 years 4 mons 1811 days 00:00:00.999999",
    "168 years 6 mons -2691 days +00:00:00.99",
    "86 years 3 mons -755 days -00:00:00.000001",
    "-36 years -6 mons +2751 days -00:00:00.000001",
    "-59 years -5 mons -2727 days +00:00:00.99",
    "-71 years -2 mons +2793 days -00:00:00.000001",
    "-187 years -363 days +00:00:00.999999",
    "143 years 16 days -00:00:00.000001",
    "-288 years -4 mons -960 days +00:00:00.99",
    "273 years 1094 days 00:00:00.999999",
    "116 years -1513 days -00:00:00.123855",
    "-286 years -11 mons +952 days 00:00:00.999999",
    "218 years 4 mons 881 days 00:00:00.744625",
    "-333 years -3 mons -729 days +00:00:00.999999",
    "183 years 10 mons -672 days -00:00:00.000001",
    "62 years 2 mons -482 days -00:00:00.000001",
    "-170 years -2 mons +1957 days -00:00:00.999999",
    "246 years 10 mons -2214 days -00:00:00.000001",
    "-319 years -4 mons +57 days 00:00:00.99",
    "309 years 1 mon -493 days +00:00:00.99",
    "21 years 2 mons -873 days +00:00:00.00009",
    "-61 years -8 mons +2547 days -00:00:00.000001",
    "190 years 5 mons 364 days -00:00:00.000001",
    "321 years 4 mons 2782 days -00:00:00.497972",
    "245 years 7 mons 369 days -00:00:00.999999",
    "-147 years -1 mons -2116 days +00:00:00.670413",
    "-109 years -7 mons -967 days +00:00:00.99",
    "185 years 8 mons 502 days -00:00:00.704946",
    "-122 years -5 mons -439 days +00:00:00.675214",
    "304 years 7 mons 349 days 00:00:00.99",
    "39 years 5 mons -1842 days -00:00:00.839243",
    "3 years 6 mons 877 days 00:00:00.999999",
    "-5 years -8 mons +916 days 00:00:00.399109",
    "281 years 6 mons 2561 days -00:00:00.999999",
    "-172 years -10 mons -1618 days +00:00:00.99",
    "253 years 1 mon -60 days +00:00:00.00009",
    "287 years 8 mons -361 days +00:00:00.00009",
    "-195 years -4 mons +424 days -00:00:00.540665",
    "133 years 8 mons 655 days 00:00:00.00009",
    "-25 years -6 mons +288 days -00:00:00.999999",
    "-41 years -1 mons -1039 days +00:00:00.999999",
    "202 years 4 mons -2256 days -00:00:00.048764",
    "-239 years -8 mons +1796 days -00:00:00.000001",
    "-247 years -11 mons +2281 days 00:00:00.99",
    "-177 years -4 mons -748 days -00:00:00.999999",
    "321 years 8 mons 449 days -00:00:00.999999",
    "236 years -768 days +00:00:00.99",
    "226 years 11 mons 2486 days -00:00:00.999999",
    "-286 years -2 mons -1475 days -00:00:00.000001",
    "278 years 11 mons -871 days +00:00:00.000001",
    "-253 years -11 mons -2114 days +00:00:00.999999",
    "-151 years -10 mons +2444 days 00:00:00.99",
    "-38 years -5 mons -1390 days",
    "-105 years -7 mons +1439 days 00:00:00.99",
    "-7 years -11 mons -115 days +00:00:00.00009",
    "-4 years -1 mons -1724 days +00:00:00.999999",
    "76 years 11 mons -367 days +00:00:00.999999",
    "41 years 5 mons -2516 days -00:00:00.999999",
    "-251 years -4 mons +2651 days -00:00:00.000001",
    "43 years 1 mon 1177 days 00:00:00.000001",
    "-153 years -4 mons -1852 days +00:00:00.999999",
    "203 years 9 mons 186 days 00:00:00.99",
    "37 years -397 days +00:00:00.999999",
    "177 years 6 mons -2388 days +00:00:00.000001",
    "-232 years +2635 days 00:00:00.99",
    "322 years 2 mons 1375 days 00:00:00.000001",
    "-42 years -8 mons +714 days 00:00:00.000001",
    "-19 years +1196 days -00:00:00.999999",
    "213 years 7 mons 188 days -00:00:00.000001",
    "228 years 9 mons 948 days -00:00:00.295322",
    "7 years 11 mons 46 days -00:00:00.000001",
    "158 years 7 mons -1885 days +00:00:00.999999",
    "-222 years -9 mons -1304 days",
    "170 years 5 mons 778 days 00:00:00.99",
    "-97 years -3 mons -1901 days",
    "181 years 2 mons 2215 days 00:00:00.999999",
    "222 years 1 mon -1619 days",
    "-320 years -6 mons -2578 days -00:00:00.999999",
    "53 years 6 mons -1526 days",
    "-80 years -5 mons +391 days 00:00:00.999999",
    "18 years 9 mons -2575 days +00:00:00.00009",
    "227 years 3 mons -2195 days",
    "-57 years -7 mons -1660 days +00:00:00.999999",
    "-222 years -11 mons +2321 days -00:00:00.999999",
    "-282 years -8 mons +1445 days 00:00:00.00009",
    "331 years 10 mons -645 days",
    "-111 years -3 mons -993 days +00:00:00.99",
    "297 years -59 days +00:00:00.00009",
    "-41 years -7 mons +2880 days 00:00:00.000001",
    "-153 years -9 mons +2826 days -00:00:00.000001",
    "297 years 2 mons -1244 days +00:00:00.190871",
    "-231 years -11 mons +2978 days",
    "279 years 5 mons -700 days -00:00:00.000001",
    "1 year 7 mons -1145 days +00:00:00.99",
    "-223 years -10 mons -639 days -00:00:00.999999",
    "307 years 7 mons -1092 days -00:00:00.999999",
    "118 years 5 mons -819 days +00:00:00.99",
    "-99 years -2 mons +2867 days 00:00:00.00009",
    "-196 years -8 mons +550 days -00:00:00.129682",
    "-322 years -2 mons +792 days -00:00:00.999999",
    "28 years 5 mons 1861 days 00:00:00.99",
    "-45 years -5 mons -2347 days +00:00:00.99",
    "1 year 11 mons 1499 days 00:00:00.101013",
    "-332 years -9 mons -1034 days +00:00:00.056136",
    "-64 years -7 mons -692 days +00:00:00.00009",
    "-142 years -11 mons -2284 days -00:00:00.000001",
    "-225 years +648 days -00:00:00.000001",
    "-65 years -11 mons -1472 days +00:00:00.99",
    "320 years -1115 days +00:00:00.999999",
    "-99 years -9 mons -2712 days +00:00:00.999999",
    "141 years 2 mons 2121 days",
    "-315 years -11 mons +245 days -00:00:00.163162",
    "-15 years -10 mons +2503 days",
    "-28 years -11 mons -2049 days +00:00:00.99",
    "103 years 1 mon 2550 days 00:00:00.999999",
    "303 years 6 mons -1605 days -00:00:00.999999",
    "-80 years -1 mons -1672 days +00:00:00.999999",
    "-79 years -775 days +00:00:00.000001",
    "-252 years -4 mons -1749 days",
    "-286 years -9 mons +2493 days 00:00:00.99",
    "-302 years -1 mons -507 days +00:00:00.99",
    "-184 years -11 mons -926 days -00:00:00.786434",
    "185 years 3 mons 1661 days -00:00:00.199787",
    "-302 years -10 mons -2732 days -00:00:00.000001",
    "-228 years -7 mons -894 days +00:00:00.99",
    "47 years 9 mons -2341 days",
    "-282 years -8 mons -1902 days +00:00:00.00009",
    "199 years 9 mons -633 days",
    "227 years 5 mons 25 days 00:00:00.713653",
    "259 years 9 mons -1648 days +00:00:00.00009",
    "242 years 7 mons 1421 days 00:00:00.99",
    "-180 years -4 mons +2346 days -00:00:00.000001",
    "51 years 7 mons 1279 days 00:00:00.352233",
    "223 years 5 mons -2539 days -00:00:00.999999",
    "298 years 5 mons 1956 days -00:00:00.000001",
    "92 years 6 mons -1182 days -00:00:00.68933",
    "-23 years -1 mons -1810 days",
    "138 years 3 mons 1414 days -00:00:00.999999",
    "-277 years -3 mons -925 days -00:00:00.999999",
    "27 years 6 mons -609 days +00:00:00.00009",
    "11 years 11 mons -119 days +00:00:00.00009",
    "49 years 3 mons -2590 days -00:00:00.999999",
    "-332 years -2 mons -1761 days -00:00:00.000001",
    "-254 years -8 mons +1085 days -00:00:00.000001",
    "129 years 7 mons -83 days -00:00:00.000001",
    "147 years 3 mons -200 days +00:00:00.999999",
    "123 years 5 mons -726 days +00:00:00.00009",
    "-136 years -6 mons +1273 days -00:00:00.999999",
    "-189 years -8 mons -400 days",
    "141 years 11 mons 2150 days 00:00:00.00009",
    "60 years 9 mons -398 days +00:00:00.999999",
    "37 years 10 mons -1839 days -00:00:00.000001",
    "48 years 798 days",
    "255 years 10 mons 2787 days 00:00:00.999999",
    "250 years 5 mons -88 days -00:00:00.999999",
    "73 years 8 mons -945 days -00:00:00.999999",
    "312 years 8 mons 1135 days -00:00:00.087142",
    "95 years 7 mons 1950 days -00:00:00.000001",
    "-32 years -6 mons +1609 days",
    "21 years 6 mons 2335 days -00:00:00.999999",
    "-79 years -5 mons +307 days 00:00:00.994486",
    "159 years 6 mons 220 days -00:00:00.932972",
    "-275 years -6 mons +1879 days 00:00:00.999999",
    "-184 years -3 mons +332 days 00:00:00.99",
    "-36 years -3 mons +2015 days 00:00:00.000001",
    "-261 years -2 mons +1969 days 00:00:00.737728",
    "-178 years -4 mons +2072 days -00:00:00.000001",
    "-71 years -2268 days +00:00:00.99",
    "129 years 4 mons -739 days +00:00:00.000001",
    "132 years 10 mons -2221 days -00:00:00.000001",
    "-40 years -7 mons -62 days -00:00:00.122892",
    "15 years 6 mons -700 days +00:00:00.000001",
    "266 years 2 mons -2865 days -00:00:00.999999",
    "60 years 9 mons 2967 days -00:00:00.999999",
    "-58 years -9 mons +1554 days 00:00:00.000001",
    "105 years 3 mons 2017 days -00:00:00.000001",
    "178 years 7 mons -665 days +00:00:00.345914",
    "106 years 9 mons 2204 days 00:00:00.000001",
    "-33 years -3 mons +1843 days 00:00:00.000001",
    "48 years 4 mons 1204 days 00:00:00.999999",
    "-194 years -9 mons +800 days 00:00:00.99",
    "226 years 4 mons -2692 days +00:00:00.99",
    "15 years 2 mons -228 days +00:00:00.999999",
    "-209 years -1 mons -919 days +00:00:00.00009",
    "-261 years -6 mons -1944 days -00:00:00.32043",
    "224 years 6 mons -1449 days +00:00:00.99",
    "-330 years -6 mons -2613 days +00:00:00.99",
    "-312 years -1 mons +2732 days 00:00:00.999999",
    "-3 years -2 mons -1856 days",
    "-175 years -6 mons +2391 days -00:00:00.487481",
    "-73 years -6 mons +876 days -00:00:00.000001",
    "114 years 7 mons -806 days -00:00:00.999999",
    "-148 years -9 mons +87 days 00:00:00.000001",
    "276 years 9 mons -535 days -00:00:00.999999",
    "-240 years -10 mons -115 days +00:00:00.99",
    "251 years 2 mons -1401 days -00:00:00.999999",
    "156 years 11 mons -655 days +00:00:00.999999",
    "209 years 1 mon 2830 days 00:00:00.999999",
    "29 years 5 mons -2574 days +00:00:00.999999",
    "-178 years -2456 days +00:00:00.000001",
    "-193 years -4 mons -85 days +00:00:00.99",
    "215 years 11 mons -2613 days -00:00:00.057869",
    "-258 years -4 mons +2841 days -00:00:00.56517",
    "-90 years +570 days 00:00:00.834179",
    "124 years 9 mons 179 days 00:00:00.00009",
    "-212 years -6 mons +860 days 00:00:00.999999",
    "192 years 6 mons -575 days +00:00:00.99",
    "-89 years -1863 days +00:00:00.000001",
    "232 years 3 mons 2696 days 00:00:00.999999",
    "74 years 4 mons -1887 days -00:00:00.000001",
    "86 years 3 mons 770 days 00:00:00.00009",
    "324 years 7 mons 1534 days 00:00:00.999999",
    "29 years 10 mons 2205 days",
    "-94 years -8 mons +2777 days 00:00:00.070432",
    "2 years 1 mon 825 days",
    "308 years 9 mons -134 days +00:00:00.999999",
    "171 years 3 mons 741 days 00:00:00.000001",
    "325 years 3 mons -998 days -00:00:00.999999",
    "-55 years -2931 days +00:00:00.999999",
    "-298 years -2 mons +2459 days 00:00:00.00009",
    "105 years 2 mons -2601 days -00:00:00.999999",
    "304 years 3 mons 1155 days 00:00:00.000001",
    "-181 years -2 mons +445 days 00:00:00.99",
    "-166 years -10 mons +828 days -00:00:00.000001",
    "-302 years -11 mons -2199 days -00:00:00.000001",
    "-209 years -2 mons -2225 days +00:00:00.00009",
    "246 years 2 mons 879 days 00:00:00.99",
    "-205 years -5 mons +2695 days 00:00:00.999999",
    "-319 years -7 mons +1210 days -00:00:00.999999",
    "-193 years -8 mons -2492 days -00:00:00.182176",
    "21 years 6 mons 678 days 00:00:00.99",
    "208 years 2 mons 101 days 00:00:00.99",
    "93 years 8 mons 1819 days 00:00:00.000001",
    "-269 years -8 mons +1419 days -00:00:00.999999",
    "29 years 11 mons 1418 days 00:00:00.000001",
    "253 years 7 mons -2616 days +00:00:00.99",
    "-283 years -5 mons +2942 days -00:00:00.000001",
    "218 years 4 mons 140 days",
    "165 years 10 mons 2791 days -00:00:00.000001",
    "-206 years -4 mons +160 days 00:00:00.00009",
    "-327 years -4 mons +2760 days",
    "33 years 8 mons -2237 days",
    "296 years 5 mons 621 days 00:00:00.99",
    "136 years 4 mons -2771 days -00:00:00.000001",
    "185 years 2 mons 2212 days 00:00:00.000001",
    "327 years 3 mons 2964 days 00:00:00.99",
];

/// PostgreSQL's answer for the total of the corpus.
const SUM_TOTAL: &str = "3720 years 5 mons -262 days +00:00:54.541936";

/// The intervals the multiplication and division corpus scales, as
/// PostgreSQL prints them.
const SCALED_INTERVALS: &[&str] = &[
    "1 day",
    "-1 days",
    "1 mon",
    "1 year 2 mons 3 days 04:05:06.789",
    "1 mon -1 days +01:00:00",
    "-1 years -9 mons",
    "00:00:00.000001",
    "2147483647 days",
    "-2147483648 days",
    "178956970 years 7 mons",
    "-178956970 years -8 mons",
    "2562047788:00:54.775807",
    "-2562047788:00:54.775807",
    "23:59:59.999999",
    "1 mon 15 days",
    "3 mons",
    "7 days 12:00:00",
    "-1 mons +29 days -23:59:59",
    "00:00:00",
    "10 years",
    "1 mon 1 day 00:00:00.000001",
    "5 days 00:00:00.5",
    "-3 mons -7 days -00:00:00.000001",
    "1 mon 30 days -23:59:59.999999",
    "2 mons 1 day",
    "100 years 100 days 100:00:00",
];

/// The factors the corpus scales by, as the statement writes them.
const FACTORS: &[&str] = &[
    "0",
    "1",
    "2",
    "-3",
    "7",
    "1000000",
    "2147483647",
    "-2147483648",
    "1.5",
    "0.1",
    "-2.5",
    "0.33333333333333333333",
    "0.5",
    "1.000001",
    "3.14159",
    "1e-7",
    "1e9",
    "0.0001",
    "86400.5",
    "-0.75",
    "2.5e-10",
    "12",
    "30",
    "-1",
];

/// PostgreSQL's answers to `iv * f`, `f * iv` and `iv / f` for every interval
/// of [`SCALED_INTERVALS`] and, within it, every factor of [`FACTORS`], `None`
/// where PostgreSQL raises `interval out of range` or `division by zero`.
const SCALED: &[[Option<&str>; 3]] = &[
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("1 day"), Some("1 day"), Some("1 day")],
    [Some("2 days"), Some("2 days"), Some("12:00:00")],
    [Some("-3 days"), Some("-3 days"), Some("-08:00:00")],
    [Some("7 days"), Some("7 days"), Some("03:25:42.857143")],
    [Some("1000000 days"), Some("1000000 days"), Some("00:00:00.0864")],
    [Some("2147483647 days"), Some("2147483647 days"), Some("00:00:00.00004")],
    [Some("-2147483648 days"), Some("-2147483648 days"), Some("-00:00:00.00004")],
    [Some("1 day 12:00:00"), Some("1 day 12:00:00"), Some("16:00:00")],
    [Some("02:24:00"), Some("02:24:00"), Some("10 days")],
    [Some("-2 days -12:00:00"), Some("-2 days -12:00:00"), Some("-09:36:00")],
    [Some("08:00:00"), Some("08:00:00"), Some("3 days")],
    [Some("12:00:00"), Some("12:00:00"), Some("2 days")],
    [Some("1 day 00:00:00.0864"), Some("1 day 00:00:00.0864"), Some("23:59:59.9136")],
    [Some("3 days 03:23:53.376"), Some("3 days 03:23:53.376"), Some("07:38:21.997396")],
    [Some("00:00:00.00864"), Some("00:00:00.00864"), Some("10000000 days")],
    [Some("1000000000 days"), Some("1000000000 days"), Some("00:00:00.000086")],
    [Some("00:00:08.64"), Some("00:00:08.64"), Some("10000 days")],
    [Some("86400 days 12:00:00"), Some("86400 days 12:00:00"), Some("00:00:00.999994")],
    [Some("-18:00:00"), Some("-18:00:00"), Some("-1 days -08:00:00")],
    [Some("00:00:00.000022"), Some("00:00:00.000022"), None],
    [Some("12 days"), Some("12 days"), Some("02:00:00")],
    [Some("30 days"), Some("30 days"), Some("00:48:00")],
    [Some("-1 days"), Some("-1 days"), Some("-1 days")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("-1 days"), Some("-1 days"), Some("-1 days")],
    [Some("-2 days"), Some("-2 days"), Some("-12:00:00")],
    [Some("3 days"), Some("3 days"), Some("08:00:00")],
    [Some("-7 days"), Some("-7 days"), Some("-03:25:42.857143")],
    [Some("-1000000 days"), Some("-1000000 days"), Some("-00:00:00.0864")],
    [Some("-2147483647 days"), Some("-2147483647 days"), Some("-00:00:00.00004")],
    [None, None, Some("00:00:00.00004")],
    [Some("-1 days -12:00:00"), Some("-1 days -12:00:00"), Some("-16:00:00")],
    [Some("-02:24:00"), Some("-02:24:00"), Some("-10 days")],
    [Some("2 days 12:00:00"), Some("2 days 12:00:00"), Some("09:36:00")],
    [Some("-08:00:00"), Some("-08:00:00"), Some("-3 days")],
    [Some("-12:00:00"), Some("-12:00:00"), Some("-2 days")],
    [Some("-1 days -00:00:00.0864"), Some("-1 days -00:00:00.0864"), Some("-23:59:59.9136")],
    [Some("-3 days -03:23:53.376"), Some("-3 days -03:23:53.376"), Some("-07:38:21.997396")],
    [Some("-00:00:00.00864"), Some("-00:00:00.00864"), Some("-10000000 days")],
    [Some("-1000000000 days"), Some("-1000000000 days"), Some("-00:00:00.000086")],
    [Some("-00:00:08.64"), Some("-00:00:08.64"), Some("-10000 days")],
    [Some("-86400 days -12:00:00"), Some("-86400 days -12:00:00"), Some("-00:00:00.999994")],
    [Some("18:00:00"), Some("18:00:00"), Some("1 day 08:00:00")],
    [Some("-00:00:00.000022"), Some("-00:00:00.000022"), None],
    [Some("-12 days"), Some("-12 days"), Some("-02:00:00")],
    [Some("-30 days"), Some("-30 days"), Some("-00:48:00")],
    [Some("1 day"), Some("1 day"), Some("1 day")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("1 mon"), Some("1 mon"), Some("1 mon")],
    [Some("2 mons"), Some("2 mons"), Some("15 days")],
    [Some("-3 mons"), Some("-3 mons"), Some("-10 days")],
    [Some("7 mons"), Some("7 mons"), Some("4 days 06:51:25.6896")],
    [Some("83333 years 4 mons"), Some("83333 years 4 mons"), Some("00:00:02.592")],
    [Some("178956970 years 7 mons"), Some("178956970 years 7 mons"), Some("00:00:00")],
    [Some("-178956970 years -8 mons"), Some("-178956970 years -8 mons"), Some("00:00:00")],
    [Some("1 mon 15 days"), Some("1 mon 15 days"), Some("20 days")],
    [Some("3 days"), Some("3 days"), Some("10 mons")],
    [Some("-2 mons -15 days"), Some("-2 mons -15 days"), Some("-12 days")],
    [Some("10 days"), Some("10 days"), Some("3 mons")],
    [Some("15 days"), Some("15 days"), Some("2 mons")],
    [Some("1 mon 00:00:02.592"), Some("1 mon 00:00:02.592"), Some("29 days 23:59:57.408")],
    [
        Some("3 mons 4 days 05:56:41.28"),
        Some("3 mons 4 days 05:56:41.28"),
        Some("9 days 13:10:59.952"),
    ],
    [Some("00:00:00.2592"), Some("00:00:00.2592"), Some("833333 years 4 mons")],
    [Some("83333333 years 4 mons"), Some("83333333 years 4 mons"), Some("00:00:00")],
    [Some("00:04:19.2"), Some("00:04:19.2"), Some("833 years 4 mons")],
    [Some("7200 years 15 days"), Some("7200 years 15 days"), Some("00:00:29.9808")],
    [Some("-22 days -12:00:00"), Some("-22 days -12:00:00"), Some("-1 mons -10 days")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("1 year"), Some("1 year"), Some("2 days 12:00:00")],
    [Some("2 years 6 mons"), Some("2 years 6 mons"), Some("1 day")],
    [Some("-1 mons"), Some("-1 mons"), Some("-1 mons")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("1 year 2 mons 3 days 04:05:06.789"),
        Some("1 year 2 mons 3 days 04:05:06.789"),
        Some("1 year 2 mons 3 days 04:05:06.789"),
    ],
    [
        Some("2 years 4 mons 6 days 08:10:13.578"),
        Some("2 years 4 mons 6 days 08:10:13.578"),
        Some("7 mons 1 day 14:02:33.3945"),
    ],
    [
        Some("-3 years -6 mons -9 days -12:15:20.367"),
        Some("-3 years -6 mons -9 days -12:15:20.367"),
        Some("-4 mons -21 days -01:21:42.263"),
    ],
    [
        Some("8 years 2 mons 21 days 28:35:47.523"),
        Some("8 years 2 mons 21 days 28:35:47.523"),
        Some("2 mons 10:52:09.541286"),
    ],
    [
        Some("1166666 years 8 mons 3000000 days 4085219:10:00"),
        Some("1166666 years 8 mons 3000000 days 4085219:10:00"),
        Some("00:00:36.561907"),
    ],
    [None, None, Some("00:00:00.000128")],
    [None, None, Some("-00:00:00.000128")],
    [
        Some("1 year 9 mons 4 days 18:07:40.1835"),
        Some("1 year 9 mons 4 days 18:07:40.1835"),
        Some("9 mons 12 days 02:43:24.526"),
    ],
    [
        Some("1 mon 12 days 07:36:30.6789"),
        Some("1 mon 12 days 07:36:30.6789"),
        Some("11 years 8 mons 30 days 40:51:07.89"),
    ],
    [
        Some("-2 years -11 mons -7 days -22:12:46.9725"),
        Some("-2 years -11 mons -7 days -22:12:46.9725"),
        Some("-5 mons -19 days -06:26:02.7156"),
    ],
    [
        Some("4 mons 21 days 01:21:42.263"),
        Some("4 mons 21 days 01:21:42.263"),
        Some("3 years 6 mons 9 days 12:15:20.367"),
    ],
    [
        Some("7 mons 1 day 14:02:33.3945"),
        Some("7 mons 1 day 14:02:33.3945"),
        Some("2 years 4 mons 6 days 08:10:13.578"),
    ],
    [
        Some("1 year 2 mons 3 days 04:05:43.350907"),
        Some("1 year 2 mons 3 days 04:05:43.350907"),
        Some("1 year 1 mon 32 days 28:04:30.227093"),
    ],
    [
        Some("3 years 7 mons 38 days 34:15:20.749255"),
        Some("3 years 7 mons 38 days 34:15:20.749255"),
        Some("4 mons 14 days 16:47:06.208476"),
    ],
    [
        Some("00:00:03.656191"),
        Some("00:00:03.656191"),
        Some("11666666 years 8 mons 30000000 days 40852191:40:00"),
    ],
    [None, None, Some("00:00:00.000274")],
    [
        Some("01:00:56.190679"),
        Some("01:00:56.190679"),
        Some("11666 years 8 mons 30000 days 40852:11:30"),
    ],
    [
        Some("100800 years 7 mons 259201 days 352976:58:42.9945"),
        Some("100800 years 7 mons 259201 days 352976:58:42.9945"),
        Some("00:07:03.160599"),
    ],
    [
        Some("-10 mons -17 days -09:03:50.09175"),
        Some("-10 mons -17 days -09:03:50.09175"),
        Some("-1 years -6 mons -24 days -05:26:49.052"),
    ],
    [Some("00:00:00.000069"), Some("00:00:00.000069"), None],
    [
        Some("14 years 36 days 49:01:21.468"),
        Some("14 years 36 days 49:01:21.468"),
        Some("1 mon 5 days 06:20:25.56575"),
    ],
    [
        Some("35 years 90 days 122:33:23.67"),
        Some("35 years 90 days 122:33:23.67"),
        Some("14 days 02:32:10.2263"),
    ],
    [
        Some("-1 years -2 mons -3 days -04:05:06.789"),
        Some("-1 years -2 mons -3 days -04:05:06.789"),
        Some("-1 years -2 mons -3 days -04:05:06.789"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("1 mon -1 days +01:00:00"),
        Some("1 mon -1 days +01:00:00"),
        Some("1 mon -1 days +01:00:00"),
    ],
    [Some("2 mons -2 days +02:00:00"), Some("2 mons -2 days +02:00:00"), Some("15 days -11:30:00")],
    [
        Some("-3 mons +3 days -03:00:00"),
        Some("-3 mons +3 days -03:00:00"),
        Some("-10 days +07:40:00"),
    ],
    [
        Some("7 mons -7 days +07:00:00"),
        Some("7 mons -7 days +07:00:00"),
        Some("4 days 03:34:17.118171"),
    ],
    [
        Some("83333 years 4 mons -1000000 days +1000000:00:00"),
        Some("83333 years 4 mons -1000000 days +1000000:00:00"),
        Some("00:00:02.5092"),
    ],
    [
        Some("178956970 years 7 mons -2147483647 days +2147483647:00:00"),
        Some("178956970 years 7 mons -2147483647 days +2147483647:00:00"),
        Some("-00:00:00.000038"),
    ],
    [None, None, Some("00:00:00.000038")],
    [Some("1 mon 14 days -10:30:00"), Some("1 mon 14 days -10:30:00"), Some("20 days -15:20:00")],
    [Some("3 days -02:18:00"), Some("3 days -02:18:00"), Some("10 mons -10 days +10:00:00")],
    [
        Some("-2 mons -13 days +09:30:00"),
        Some("-2 mons -13 days +09:30:00"),
        Some("-12 days +09:12:00"),
    ],
    [Some("10 days -07:40:00"), Some("10 days -07:40:00"), Some("3 mons -3 days +03:00:00")],
    [Some("15 days -11:30:00"), Some("15 days -11:30:00"), Some("2 mons -2 days +02:00:00")],
    [
        Some("1 mon -1 days +01:00:02.5092"),
        Some("1 mon -1 days +01:00:02.5092"),
        Some("29 days 00:59:57.4908"),
    ],
    [
        Some("3 mons 1 day 05:41:17.628"),
        Some("3 mons 1 day 05:41:17.628"),
        Some("9 days 05:51:43.871162"),
    ],
    [
        Some("00:00:00.25092"),
        Some("00:00:00.25092"),
        Some("833333 years 4 mons -10000000 days +10000000:00:00"),
    ],
    [
        Some("83333333 years 4 mons -1000000000 days +1000000000:00:00"),
        Some("83333333 years 4 mons -1000000000 days +1000000000:00:00"),
        Some("-00:00:00.000082"),
    ],
    [Some("00:04:10.92"), Some("00:04:10.92"), Some("833 years 4 mons -10000 days +10000:00:00")],
    [
        Some("7200 years -86385 days +86388:30:00"),
        Some("7200 years -86385 days +86388:30:00"),
        Some("00:00:29.022472"),
    ],
    [Some("-22 days +05:15:00"), Some("-22 days +05:15:00"), Some("-1 mons -9 days +06:40:00")],
    [Some("-00:00:00.000021"), Some("-00:00:00.000021"), None],
    [Some("1 year -12 days +12:00:00"), Some("1 year -12 days +12:00:00"), Some("2 days 10:05:00")],
    [
        Some("2 years 6 mons -30 days +30:00:00"),
        Some("2 years 6 mons -30 days +30:00:00"),
        Some("1 day -00:46:00"),
    ],
    [
        Some("-1 mons +1 day -01:00:00"),
        Some("-1 mons +1 day -01:00:00"),
        Some("-1 mons +1 day -01:00:00"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("-1 years -9 mons"), Some("-1 years -9 mons"), Some("-1 years -9 mons")],
    [Some("-3 years -6 mons"), Some("-3 years -6 mons"), Some("-10 mons -15 days")],
    [Some("5 years 3 mons"), Some("5 years 3 mons"), Some("7 mons")],
    [Some("-12 years -3 mons"), Some("-12 years -3 mons"), Some("-3 mons")],
    [Some("-1750000 years"), Some("-1750000 years"), Some("-00:00:54.432")],
    [None, None, Some("00:00:00")],
    [None, None, Some("00:00:00")],
    [
        Some("-2 years -7 mons -15 days"),
        Some("-2 years -7 mons -15 days"),
        Some("-1 years -2 mons"),
    ],
    [Some("-2 mons -3 days"), Some("-2 mons -3 days"), Some("-17 years -6 mons")],
    [Some("4 years 4 mons 15 days"), Some("4 years 4 mons 15 days"), Some("8 mons 12 days")],
    [Some("-7 mons"), Some("-7 mons"), Some("-5 years -3 mons")],
    [Some("-10 mons -15 days"), Some("-10 mons -15 days"), Some("-3 years -6 mons")],
    [
        Some("-1 years -9 mons -00:00:54.432"),
        Some("-1 years -9 mons -00:00:54.432"),
        Some("-1 years -8 mons -29 days -23:59:05.568"),
    ],
    [
        Some("-5 years -5 mons -29 days -04:50:26.88"),
        Some("-5 years -5 mons -29 days -04:50:26.88"),
        Some("-6 mons -20 days -12:50:58.3872"),
    ],
    [Some("-00:00:05.4432"), Some("-00:00:05.4432"), Some("-17500000 years")],
    [None, None, Some("-00:00:00.0864")],
    [Some("-01:30:43.2"), Some("-01:30:43.2"), Some("-17500 years")],
    [
        Some("-151200 years -10 mons -15 days"),
        Some("-151200 years -10 mons -15 days"),
        Some("-00:10:30.0288"),
    ],
    [
        Some("1 year 3 mons 22 days 12:00:00"),
        Some("1 year 3 mons 22 days 12:00:00"),
        Some("2 years 4 mons"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("-21 years"), Some("-21 years"), Some("-1 mons -22 days -12:00:00")],
    [Some("-52 years -6 mons"), Some("-52 years -6 mons"), Some("-21 days")],
    [Some("1 year 9 mons"), Some("1 year 9 mons"), Some("1 year 9 mons")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("00:00:00.000001"), Some("00:00:00.000001"), Some("00:00:00.000001")],
    [Some("00:00:00.000002"), Some("00:00:00.000002"), Some("00:00:00")],
    [Some("-00:00:00.000003"), Some("-00:00:00.000003"), Some("00:00:00")],
    [Some("00:00:00.000007"), Some("00:00:00.000007"), Some("00:00:00")],
    [Some("00:00:01"), Some("00:00:01"), Some("00:00:00")],
    [Some("00:35:47.483647"), Some("00:35:47.483647"), Some("00:00:00")],
    [Some("-00:35:47.483648"), Some("-00:35:47.483648"), Some("00:00:00")],
    [Some("00:00:00.000002"), Some("00:00:00.000002"), Some("00:00:00.000001")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00.00001")],
    [Some("-00:00:00.000002"), Some("-00:00:00.000002"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00.000003")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00.000002")],
    [Some("00:00:00.000001"), Some("00:00:00.000001"), Some("00:00:00.000001")],
    [Some("00:00:00.000003"), Some("00:00:00.000003"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:10")],
    [Some("00:16:40"), Some("00:16:40"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00.01")],
    [Some("00:00:00.0864"), Some("00:00:00.0864"), Some("00:00:00")],
    [Some("-00:00:00.000001"), Some("-00:00:00.000001"), Some("-00:00:00.000001")],
    [Some("00:00:00"), Some("00:00:00"), Some("01:06:40")],
    [Some("00:00:00.000012"), Some("00:00:00.000012"), Some("00:00:00")],
    [Some("00:00:00.00003"), Some("00:00:00.00003"), Some("00:00:00")],
    [Some("-00:00:00.000001"), Some("-00:00:00.000001"), Some("-00:00:00.000001")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("2147483647 days"), Some("2147483647 days"), Some("2147483647 days")],
    [None, None, Some("1073741823 days 12:00:00")],
    [None, None, Some("-715827882 days -08:00:00.003433")],
    [None, None, Some("306783378 days 03:25:42.856407")],
    [None, None, Some("2147 days 11:36:27.1008")],
    [None, None, Some("1 day")],
    [None, None, Some("-23:59:59.99996")],
    [None, None, Some("1431655764 days 16:00:00.006866")],
    [Some("214748364 days 16:48:00.001545"), Some("214748364 days 16:48:00.001545"), None],
    [None, None, Some("-858993458 days -19:11:59.99588")],
    [Some("715827882 days 07:59:59.993134"), Some("715827882 days 07:59:59.993134"), None],
    [Some("1073741823 days 12:00:00"), Some("1073741823 days 12:00:00"), None],
    [None, None, Some("2147481499 days 12:26:38.448944")],
    [None, None, Some("683565852 days 15:23:45.331879")],
    [Some("214 days 17:57:38.71008"), Some("214 days 17:57:38.71008"), None],
    [None, None, Some("2 days 03:32:22.587101")],
    [Some("214748 days 08:45:10.08"), Some("214748 days 08:45:10.08"), None],
    [None, None, Some("24854 days 23:46:59.504517")],
    [Some("-1610612735 days -06:00:00"), Some("-1610612735 days -06:00:00"), None],
    [Some("12:53:05.646775"), Some("12:53:05.646775"), None],
    [None, None, Some("178956970 days 14:00:00.000858")],
    [None, None, Some("71582788 days 05:36:00.000086")],
    [Some("-2147483647 days"), Some("-2147483647 days"), Some("-2147483647 days")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("-2147483648 days"), Some("-2147483648 days"), Some("-2147483648 days")],
    [None, None, Some("-1073741824 days")],
    [None, None, Some("715827882 days 15:59:59.996567")],
    [None, None, Some("-306783378 days -06:51:25.712814")],
    [None, None, Some("-2147 days -11:36:27.1872")],
    [None, None, Some("-1 days -00:00:00.00004")],
    [None, None, Some("1 day")],
    [None, None, Some("-1431655765 days -07:59:59.993134")],
    [Some("-214748364 days -19:12:00.00103"), Some("-214748364 days -19:12:00.00103"), None],
    [None, None, Some("858993459 days 04:48:00.00412")],
    [Some("-715827882 days -15:59:59.996567"), Some("-715827882 days -15:59:59.996567"), None],
    [Some("-1073741824 days"), Some("-1073741824 days"), None],
    [None, None, Some("-2147481500 days -12:26:38.366547")],
    [None, None, Some("-683565852 days -23:02:07.32811")],
    [Some("-214 days -17:57:38.71872"), Some("-214 days -17:57:38.71872"), None],
    [None, None, Some("-2 days -03:32:22.587187")],
    [Some("-214748 days -08:45:18.720001"), Some("-214748 days -08:45:18.720001"), None],
    [None, None, Some("-24854 days -23:47:00.504511")],
    [Some("1610612736 days"), Some("1610612736 days"), None],
    [Some("-12:53:05.646797"), Some("-12:53:05.646797"), None],
    [None, None, Some("-178956970 days -15:59:59.999142")],
    [None, None, Some("-71582788 days -06:23:59.999914")],
    [None, None, None],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("178956970 years 7 mons"),
        Some("178956970 years 7 mons"),
        Some("178956970 years 7 mons"),
    ],
    [None, None, Some("89478485 years 3 mons 15 days")],
    [None, None, Some("-59652323 years -6 mons -10 days -00:00:00.0864")],
    [None, None, Some("25565281 years 6 mons 4 days 06:51:25.6896")],
    [None, None, Some("178 years 11 mons 14 days 12:13:33.024")],
    [None, None, Some("1 mon")],
    [None, None, Some("-30 days")],
    [None, None, Some("119304647 years 20 days 00:00:00.1728")],
    [
        Some("17895697 years 21 days 00:00:00.0864"),
        Some("17895697 years 21 days 00:00:00.0864"),
        None,
    ],
    [None, None, Some("-71582788 years -2 mons -23 days -23:59:59.9136")],
    [
        Some("59652323 years 6 mons 9 days 23:59:59.8272"),
        Some("59652323 years 6 mons 9 days 23:59:59.8272"),
        None,
    ],
    [Some("89478485 years 3 mons 15 days"), Some("89478485 years 3 mons 15 days"), None],
    [None, None, Some("178956791 years 7 mons 15 days 13:19:13.4688")],
    [None, None, Some("56963821 years 19 days 05:52:39.9648")],
    [
        Some("17 years 10 mons 22 days 10:49:21.3024"),
        Some("17 years 10 mons 22 days 10:49:21.3024"),
        None,
    ],
    [None, None, Some("2 mons 4 days 10:11:17.5776")],
    [
        Some("17895 years 8 mons 10 days 22:35:02.4"),
        Some("17895 years 8 mons 10 days 22:35:02.4"),
        None,
    ],
    [None, None, Some("2071 years 2 mons 29 days 17:29:45.168")],
    [
        Some("-134217727 years -11 mons -7 days -12:00:00"),
        Some("-134217727 years -11 mons -7 days -12:00:00"),
        None,
    ],
    [Some("16 days 02:32:49.3728"), Some("16 days 02:32:49.3728"), None],
    [None, None, Some("14913080 years 10 mons 17 days 12:00:00")],
    [None, None, Some("5965232 years 4 mons 7 days")],
    [
        Some("-178956970 years -7 mons"),
        Some("-178956970 years -7 mons"),
        Some("-178956970 years -7 mons"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("-178956970 years -8 mons"),
        Some("-178956970 years -8 mons"),
        Some("-178956970 years -8 mons"),
    ],
    [None, None, Some("-89478485 years -4 mons")],
    [None, None, Some("59652323 years 6 mons 19 days 23:59:59.9136")],
    [None, None, Some("-25565281 years -6 mons -8 days -13:42:51.3792")],
    [None, None, Some("-178 years -11 mons -14 days -12:13:35.616")],
    [None, None, Some("-1 mons")],
    [None, None, Some("1 mon")],
    [None, None, Some("-119304647 years -1 mons -9 days -23:59:59.8272")],
    [Some("-17895697 years -24 days"), Some("-17895697 years -24 days"), None],
    [None, None, Some("71582788 years 3 mons 6 days 00:00:00.0864")],
    [
        Some("-59652323 years -6 mons -19 days -23:59:59.9136"),
        Some("-59652323 years -6 mons -19 days -23:59:59.9136"),
        None,
    ],
    [Some("-89478485 years -4 mons"), Some("-89478485 years -4 mons"), None],
    [None, None, Some("-178956791 years -8 mons -15 days -13:19:10.9632")],
    [None, None, Some("-56963821 years -28 days -19:03:39.8304")],
    [
        Some("-17 years -10 mons -22 days -10:49:21.5616"),
        Some("-17 years -10 mons -22 days -10:49:21.5616"),
        None,
    ],
    [None, None, Some("-2 mons -4 days -10:11:17.5776")],
    [
        Some("-17895 years -8 mons -10 days -22:39:21.6"),
        Some("-17895 years -8 mons -10 days -22:39:21.6"),
        None,
    ],
    [None, None, Some("-2071 years -2 mons -29 days -17:30:15.1488")],
    [Some("134217728 years"), Some("134217728 years"), None],
    [Some("-16 days -02:32:49.3728"), Some("-16 days -02:32:49.3728"), None],
    [None, None, Some("-14913080 years -10 mons -20 days")],
    [None, None, Some("-5965232 years -4 mons -8 days")],
    [None, None, None],
    [Some("00:00:00"), Some("00:00:00"), None],
    [None, None, None],
    [None, None, Some("1281023894:00:27.387904")],
    [None, None, Some("-854015929:20:18.258432")],
    [None, None, Some("366006826:51:33.539328")],
    [None, None, Some("2562:02:52.036855")],
    [None, None, Some("01:11:34.967298")],
    [None, None, Some("-01:11:34.967296")],
    [None, None, Some("1708031858:40:36.516864")],
    [Some("256204778:48:05.477632"), Some("256204778:48:05.477632"), None],
    [None, None, Some("-1024819115:12:21.910528")],
    [Some("854015929:20:18.258432"), Some("854015929:20:18.258432"), None],
    [Some("1281023894:00:27.387904"), Some("1281023894:00:27.387904"), None],
    [None, None, Some("2562045225:58:11.96288")],
    [None, None, Some("815525828:38:44.716032")],
    [Some("256:12:17.203685"), Some("256:12:17.203685"), None],
    [None, None, Some("02:33:43.372037")],
    [Some("256204:46:43.685478"), Some("256204:46:43.685478"), None],
    [None, None, Some("29653:09:33.393149")],
    [Some("-1921535841:00:41.081856"), Some("-1921535841:00:41.081856"), None],
    [Some("00:38:25.843009"), Some("00:38:25.843009"), None],
    [None, None, Some("213503982:20:04.564608")],
    [None, None, Some("85401592:56:01.825856")],
    [
        Some("-2562047788:00:54.775808"),
        Some("-2562047788:00:54.775808"),
        Some("-2562047788:00:54.775808"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("-2562047788:00:54.775808"),
        Some("-2562047788:00:54.775808"),
        Some("-2562047788:00:54.775808"),
    ],
    [None, None, Some("-1281023894:00:27.387904")],
    [None, None, Some("854015929:20:18.258432")],
    [None, None, Some("-366006826:51:33.539328")],
    [None, None, Some("-2562:02:52.036855")],
    [None, None, Some("-01:11:34.967298")],
    [None, None, Some("01:11:34.967296")],
    [None, None, Some("-1708031858:40:36.516864")],
    [Some("-256204778:48:05.477632"), Some("-256204778:48:05.477632"), None],
    [None, None, Some("1024819115:12:21.910528")],
    [Some("-854015929:20:18.258432"), Some("-854015929:20:18.258432"), None],
    [Some("-1281023894:00:27.387904"), Some("-1281023894:00:27.387904"), None],
    [None, None, Some("-2562045225:58:11.96288")],
    [None, None, Some("-815525828:38:44.716032")],
    [Some("-256:12:17.203685"), Some("-256:12:17.203685"), None],
    [None, None, Some("-02:33:43.372037")],
    [Some("-256204:46:43.685478"), Some("-256204:46:43.685478"), None],
    [None, None, Some("-29653:09:33.393149")],
    [Some("1921535841:00:41.081856"), Some("1921535841:00:41.081856"), None],
    [Some("-00:38:25.843009"), Some("-00:38:25.843009"), None],
    [None, None, Some("-213503982:20:04.564608")],
    [None, None, Some("-85401592:56:01.825856")],
    [None, None, None],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("23:59:59.999999"), Some("23:59:59.999999"), Some("23:59:59.999999")],
    [Some("47:59:59.999998"), Some("47:59:59.999998"), Some("12:00:00")],
    [Some("-71:59:59.999997"), Some("-71:59:59.999997"), Some("-08:00:00")],
    [Some("167:59:59.999993"), Some("167:59:59.999993"), Some("03:25:42.857143")],
    [Some("23999999:59:59"), Some("23999999:59:59"), Some("00:00:00.0864")],
    [None, None, Some("00:00:00.00004")],
    [None, None, Some("-00:00:00.00004")],
    [Some("35:59:59.999998"), Some("35:59:59.999998"), Some("15:59:59.999999")],
    [Some("02:24:00"), Some("02:24:00"), Some("239:59:59.99999")],
    [Some("-59:59:59.999998"), Some("-59:59:59.999998"), Some("-09:36:00")],
    [Some("08:00:00"), Some("08:00:00"), Some("71:59:59.999997")],
    [Some("12:00:00"), Some("12:00:00"), Some("47:59:59.999998")],
    [Some("24:00:00.086399"), Some("24:00:00.086399"), Some("23:59:59.913599")],
    [Some("75:23:53.375997"), Some("75:23:53.375997"), Some("07:38:21.997396")],
    [Some("00:00:00.00864"), Some("00:00:00.00864"), Some("239999999:59:50")],
    [None, None, Some("00:00:00.000086")],
    [Some("00:00:08.64"), Some("00:00:08.64"), Some("239999:59:59.99")],
    [Some("2073611:59:59.9136"), Some("2073611:59:59.9136"), Some("00:00:00.999994")],
    [Some("-17:59:59.999999"), Some("-17:59:59.999999"), Some("-31:59:59.999999")],
    [Some("00:00:00.000022"), Some("00:00:00.000022"), None],
    [Some("287:59:59.999988"), Some("287:59:59.999988"), Some("02:00:00")],
    [Some("719:59:59.99997"), Some("719:59:59.99997"), Some("00:48:00")],
    [Some("-23:59:59.999999"), Some("-23:59:59.999999"), Some("-23:59:59.999999")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("1 mon 15 days"), Some("1 mon 15 days"), Some("1 mon 15 days")],
    [Some("2 mons 30 days"), Some("2 mons 30 days"), Some("22 days 12:00:00")],
    [Some("-3 mons -45 days"), Some("-3 mons -45 days"), Some("-15 days")],
    [Some("7 mons 105 days"), Some("7 mons 105 days"), Some("6 days 10:17:08.546743")],
    [
        Some("83333 years 4 mons 15000000 days"),
        Some("83333 years 4 mons 15000000 days"),
        Some("00:00:03.888"),
    ],
    [None, None, Some("00:00:00.000603")],
    [None, None, Some("-00:00:00.000603")],
    [Some("1 mon 37 days 12:00:00"), Some("1 mon 37 days 12:00:00"), Some("30 days")],
    [Some("4 days 12:00:00"), Some("4 days 12:00:00"), Some("10 mons 150 days")],
    [Some("-2 mons -52 days -12:00:00"), Some("-2 mons -52 days -12:00:00"), Some("-18 days")],
    [Some("15 days"), Some("15 days"), Some("3 mons 45 days")],
    [Some("22 days 12:00:00"), Some("22 days 12:00:00"), Some("2 mons 30 days")],
    [
        Some("1 mon 15 days 00:00:03.888"),
        Some("1 mon 15 days 00:00:03.888"),
        Some("44 days 23:59:56.112001"),
    ],
    [
        Some("3 mons 51 days 08:55:01.92"),
        Some("3 mons 51 days 08:55:01.92"),
        Some("14 days 07:46:29.912943"),
    ],
    [Some("00:00:00.3888"), Some("00:00:00.3888"), Some("833333 years 4 mons 150000000 days")],
    [None, None, Some("00:00:00.001296")],
    [Some("00:06:28.8"), Some("00:06:28.8"), Some("833 years 4 mons 150000 days")],
    [
        Some("7200 years 1296022 days 12:00:00"),
        Some("7200 years 1296022 days 12:00:00"),
        Some("00:00:44.980713"),
    ],
    [Some("-33 days -18:00:00"), Some("-33 days -18:00:00"), Some("-1 mons -30 days")],
    [Some("00:00:00.000324"), Some("00:00:00.000324"), None],
    [Some("1 year 180 days"), Some("1 year 180 days"), Some("3 days 18:00:00")],
    [Some("2 years 6 mons 450 days"), Some("2 years 6 mons 450 days"), Some("1 day 12:00:00")],
    [Some("-1 mons -15 days"), Some("-1 mons -15 days"), Some("-1 mons -15 days")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("3 mons"), Some("3 mons"), Some("3 mons")],
    [Some("6 mons"), Some("6 mons"), Some("1 mon 15 days")],
    [Some("-9 mons"), Some("-9 mons"), Some("-1 mons")],
    [Some("1 year 9 mons"), Some("1 year 9 mons"), Some("12 days 20:34:17.1552")],
    [Some("250000 years"), Some("250000 years"), Some("00:00:07.776")],
    [None, None, Some("00:00:00")],
    [None, None, Some("00:00:00")],
    [Some("4 mons 15 days"), Some("4 mons 15 days"), Some("2 mons")],
    [Some("9 days"), Some("9 days"), Some("2 years 6 mons")],
    [Some("-7 mons -15 days"), Some("-7 mons -15 days"), Some("-1 mons -6 days")],
    [Some("1 mon"), Some("1 mon"), Some("9 mons")],
    [Some("1 mon 15 days"), Some("1 mon 15 days"), Some("6 mons")],
    [Some("3 mons 00:00:07.776"), Some("3 mons 00:00:07.776"), Some("2 mons 29 days 23:59:52.224")],
    [
        Some("9 mons 12 days 17:50:03.84"),
        Some("9 mons 12 days 17:50:03.84"),
        Some("28 days 15:32:59.7696"),
    ],
    [Some("00:00:00.7776"), Some("00:00:00.7776"), Some("2500000 years")],
    [None, None, Some("00:00:00")],
    [Some("00:12:57.6"), Some("00:12:57.6"), Some("2500 years")],
    [Some("21600 years 1 mon 15 days"), Some("21600 years 1 mon 15 days"), Some("00:01:30.0288")],
    [Some("-2 mons -7 days -12:00:00"), Some("-2 mons -7 days -12:00:00"), Some("-4 mons")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("3 years"), Some("3 years"), Some("7 days 12:00:00")],
    [Some("7 years 6 mons"), Some("7 years 6 mons"), Some("3 days")],
    [Some("-3 mons"), Some("-3 mons"), Some("-3 mons")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("7 days 12:00:00"), Some("7 days 12:00:00"), Some("7 days 12:00:00")],
    [Some("14 days 24:00:00"), Some("14 days 24:00:00"), Some("3 days 18:00:00")],
    [Some("-21 days -36:00:00"), Some("-21 days -36:00:00"), Some("-2 days -12:00:00")],
    [Some("49 days 84:00:00"), Some("49 days 84:00:00"), Some("1 day 01:42:51.428571")],
    [
        Some("7000000 days 12000000:00:00"),
        Some("7000000 days 12000000:00:00"),
        Some("00:00:00.648"),
    ],
    [None, None, Some("00:00:00.000302")],
    [None, None, Some("-00:00:00.000302")],
    [Some("10 days 30:00:00"), Some("10 days 30:00:00"), Some("4 days 24:00:00")],
    [Some("18:00:00"), Some("18:00:00"), Some("70 days 120:00:00")],
    [Some("-17 days -42:00:00"), Some("-17 days -42:00:00"), Some("-2 days -24:00:00")],
    [Some("2 days 12:00:00"), Some("2 days 12:00:00"), Some("21 days 36:00:00")],
    [Some("3 days 18:00:00"), Some("3 days 18:00:00"), Some("14 days 24:00:00")],
    [Some("7 days 12:00:00.648"), Some("7 days 12:00:00.648"), Some("6 days 35:59:59.352001")],
    [Some("21 days 61:29:10.32"), Some("21 days 61:29:10.32"), Some("2 days 09:17:44.980472")],
    [Some("00:00:00.0648"), Some("00:00:00.0648"), Some("70000000 days 120000000:00:00")],
    [None, None, Some("00:00:00.000648")],
    [Some("00:01:04.8"), Some("00:01:04.8"), Some("70000 days 120000:00:00")],
    [Some("604803 days 1036818:00:00"), Some("604803 days 1036818:00:00"), Some("00:00:07.499956")],
    [Some("-5 days -15:00:00"), Some("-5 days -15:00:00"), Some("-9 days -24:00:00")],
    [Some("00:00:00.000162"), Some("00:00:00.000162"), None],
    [Some("84 days 144:00:00"), Some("84 days 144:00:00"), Some("15:00:00")],
    [Some("210 days 360:00:00"), Some("210 days 360:00:00"), Some("06:00:00")],
    [Some("-7 days -12:00:00"), Some("-7 days -12:00:00"), Some("-7 days -12:00:00")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("-1 mons +29 days -23:59:59"),
        Some("-1 mons +29 days -23:59:59"),
        Some("-1 mons +29 days -23:59:59"),
    ],
    [
        Some("-2 mons +58 days -47:59:58"),
        Some("-2 mons +58 days -47:59:58"),
        Some("-1 days +00:00:00.5"),
    ],
    [
        Some("3 mons -87 days +71:59:57"),
        Some("3 mons -87 days +71:59:57"),
        Some("1 day -08:00:00.333333"),
    ],
    [
        Some("-7 mons +203 days -167:59:53"),
        Some("-7 mons +203 days -167:59:53"),
        Some("-06:51:25.546743"),
    ],
    [
        Some("-83333 years -4 mons +29000000 days -23999722:13:20"),
        Some("-83333 years -4 mons +29000000 days -23999722:13:20"),
        Some("-00:00:00.172799"),
    ],
    [None, None, Some("00:00:00.001127")],
    [None, None, Some("-00:00:00.001127")],
    [
        Some("-1 mons +28 days -23:59:58.5"),
        Some("-1 mons +28 days -23:59:58.5"),
        Some("-1 days -07:59:59.333333"),
    ],
    [
        Some("-1 days +19:12:00.1"),
        Some("-1 days +19:12:00.1"),
        Some("-10 mons +290 days -239:59:50"),
    ],
    [
        Some("2 mons -57 days +47:59:57.5"),
        Some("2 mons -57 days +47:59:57.5"),
        Some("1 day -04:48:00.4"),
    ],
    [
        Some("-1 days +08:00:00.333333"),
        Some("-1 days +08:00:00.333333"),
        Some("-3 mons +87 days -71:59:57"),
    ],
    [Some("-1 days +00:00:00.5"), Some("-1 days +00:00:00.5"), Some("-2 mons +58 days -47:59:58")],
    [
        Some("-1 mons +29 days -23:59:59.172799"),
        Some("-1 mons +29 days -23:59:59.172799"),
        Some("-1 days -23:59:58.827198"),
    ],
    [
        Some("-3 mons +87 days -78:47:43.61041"),
        Some("-3 mons +87 days -78:47:43.61041"),
        Some("-15:16:43.706596"),
    ],
    [
        Some("-00:00:00.01728"),
        Some("-00:00:00.01728"),
        Some("-833333 years -4 mons +290000000 days -239997222:13:20"),
    ],
    [None, None, Some("00:00:00.00242")],
    [
        Some("-00:00:17.2799"),
        Some("-00:00:17.2799"),
        Some("-833 years -4 mons +290000 days -239997:13:20"),
    ],
    [
        Some("-7200 years +2505599 days -2073575:59:59.5"),
        Some("-7200 years +2505599 days -2073575:59:59.5"),
        Some("-00:00:01.980951"),
    ],
    [Some("1 day 11:59:59.25"), Some("1 day 11:59:59.25"), Some("1 mon -28 days +15:59:58.666667")],
    [Some("00:00:00.000604"), Some("00:00:00.000604"), None],
    [
        Some("-1 years +348 days -287:59:48"),
        Some("-1 years +348 days -287:59:48"),
        Some("-03:59:59.916667"),
    ],
    [
        Some("-2 years -6 mons +870 days -719:59:30"),
        Some("-2 years -6 mons +870 days -719:59:30"),
        Some("-1 days +22:24:00.033333"),
    ],
    [
        Some("1 mon -29 days +23:59:59"),
        Some("1 mon -29 days +23:59:59"),
        Some("1 mon -29 days +23:59:59"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("10 years"), Some("10 years"), Some("10 years")],
    [Some("20 years"), Some("20 years"), Some("5 years")],
    [Some("-30 years"), Some("-30 years"), Some("-3 years -4 mons")],
    [Some("70 years"), Some("70 years"), Some("1 year 5 mons 4 days 06:51:25.6896")],
    [Some("10000000 years"), Some("10000000 years"), Some("00:05:11.04")],
    [None, None, Some("00:00:00.1728")],
    [None, None, Some("-00:00:00.1728")],
    [Some("15 years"), Some("15 years"), Some("6 years 8 mons")],
    [Some("1 year"), Some("1 year"), Some("100 years")],
    [Some("-25 years"), Some("-25 years"), Some("-4 years")],
    [Some("3 years 4 mons"), Some("3 years 4 mons"), Some("30 years")],
    [Some("5 years"), Some("5 years"), Some("20 years")],
    [
        Some("10 years 00:05:11.04"),
        Some("10 years 00:05:11.04"),
        Some("9 years 11 mons 29 days 23:54:48.96"),
    ],
    [
        Some("31 years 4 mons 29 days 17:22:33.6"),
        Some("31 years 4 mons 29 days 17:22:33.6"),
        Some("3 years 2 mons 5 days 21:59:50.6112"),
    ],
    [Some("00:00:31.104"), Some("00:00:31.104"), Some("100000000 years")],
    [None, None, Some("00:00:00.3456")],
    [Some("08:38:24"), Some("08:38:24"), Some("100000 years")],
    [Some("864005 years"), Some("864005 years"), Some("00:59:59.9424")],
    [Some("-7 years -6 mons"), Some("-7 years -6 mons"), Some("-13 years -4 mons")],
    [Some("00:00:00.0864"), Some("00:00:00.0864"), None],
    [Some("120 years"), Some("120 years"), Some("10 mons")],
    [Some("300 years"), Some("300 years"), Some("4 mons")],
    [Some("-10 years"), Some("-10 years"), Some("-10 years")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("1 mon 1 day 00:00:00.000001"),
        Some("1 mon 1 day 00:00:00.000001"),
        Some("1 mon 1 day 00:00:00.000001"),
    ],
    [
        Some("2 mons 2 days 00:00:00.000002"),
        Some("2 mons 2 days 00:00:00.000002"),
        Some("15 days 12:00:00"),
    ],
    [
        Some("-3 mons -3 days -00:00:00.000003"),
        Some("-3 mons -3 days -00:00:00.000003"),
        Some("-10 days -08:00:00"),
    ],
    [
        Some("7 mons 7 days 00:00:00.000007"),
        Some("7 mons 7 days 00:00:00.000007"),
        Some("4 days 10:17:08.546743"),
    ],
    [
        Some("83333 years 4 mons 1000000 days 00:00:01"),
        Some("83333 years 4 mons 1000000 days 00:00:01"),
        Some("00:00:02.6784"),
    ],
    [
        Some("178956970 years 7 mons 2147483647 days 00:35:47.483647"),
        Some("178956970 years 7 mons 2147483647 days 00:35:47.483647"),
        Some("00:00:00.00004"),
    ],
    [
        Some("-178956970 years -8 mons -2147483648 days -00:35:47.483648"),
        Some("-178956970 years -8 mons -2147483648 days -00:35:47.483648"),
        Some("-00:00:00.00004"),
    ],
    [
        Some("1 mon 16 days 12:00:00.000002"),
        Some("1 mon 16 days 12:00:00.000002"),
        Some("20 days 16:00:00.000001"),
    ],
    [Some("3 days 02:24:00"), Some("3 days 02:24:00"), Some("10 mons 10 days 00:00:00.00001")],
    [
        Some("-2 mons -17 days -12:00:00.000002"),
        Some("-2 mons -17 days -12:00:00.000002"),
        Some("-12 days -09:36:00"),
    ],
    [Some("10 days 08:00:00"), Some("10 days 08:00:00"), Some("3 mons 3 days 00:00:00.000003")],
    [Some("15 days 12:00:00"), Some("15 days 12:00:00"), Some("2 mons 2 days 00:00:00.000002")],
    [
        Some("1 mon 1 day 00:00:02.678401"),
        Some("1 mon 1 day 00:00:02.678401"),
        Some("30 days 23:59:57.321601"),
    ],
    [
        Some("3 mons 7 days 09:20:34.656003"),
        Some("3 mons 7 days 09:20:34.656003"),
        Some("9 days 20:49:21.949396"),
    ],
    [
        Some("00:00:00.26784"),
        Some("00:00:00.26784"),
        Some("833333 years 4 mons 10000000 days 00:00:10"),
    ],
    [
        Some("83333333 years 4 mons 1000000000 days 00:16:40"),
        Some("83333333 years 4 mons 1000000000 days 00:16:40"),
        Some("00:00:00.000086"),
    ],
    [Some("00:04:27.84"), Some("00:04:27.84"), Some("833 years 4 mons 10000 days 00:00:00.01")],
    [
        Some("7200 years 86415 days 12:00:00.0864"),
        Some("7200 years 86415 days 12:00:00.0864"),
        Some("00:00:30.980794"),
    ],
    [
        Some("-23 days -06:00:00.000001"),
        Some("-23 days -06:00:00.000001"),
        Some("-1 mons -11 days -08:00:00.000001"),
    ],
    [Some("00:00:00.000022"), Some("00:00:00.000022"), None],
    [
        Some("1 year 12 days 00:00:00.000012"),
        Some("1 year 12 days 00:00:00.000012"),
        Some("2 days 14:00:00"),
    ],
    [
        Some("2 years 6 mons 30 days 00:00:00.00003"),
        Some("2 years 6 mons 30 days 00:00:00.00003"),
        Some("1 day 00:48:00"),
    ],
    [
        Some("-1 mons -1 days -00:00:00.000001"),
        Some("-1 mons -1 days -00:00:00.000001"),
        Some("-1 mons -1 days -00:00:00.000001"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("5 days 00:00:00.5"), Some("5 days 00:00:00.5"), Some("5 days 00:00:00.5")],
    [Some("10 days 00:00:01"), Some("10 days 00:00:01"), Some("2 days 12:00:00.25")],
    [Some("-15 days -00:00:01.5"), Some("-15 days -00:00:01.5"), Some("-1 days -16:00:00.166667")],
    [Some("35 days 00:00:03.5"), Some("35 days 00:00:03.5"), Some("17:08:34.357143")],
    [Some("5000000 days 138:53:20"), Some("5000000 days 138:53:20"), Some("00:00:00.432")],
    [None, None, Some("00:00:00.000201")],
    [None, None, Some("-00:00:00.000201")],
    [Some("7 days 12:00:00.75"), Some("7 days 12:00:00.75"), Some("3 days 08:00:00.333333")],
    [Some("12:00:00.05"), Some("12:00:00.05"), Some("50 days 00:00:05")],
    [Some("-12 days -12:00:01.25"), Some("-12 days -12:00:01.25"), Some("-2 days -00:00:00.2")],
    [Some("1 day 16:00:00.166667"), Some("1 day 16:00:00.166667"), Some("15 days 00:00:01.5")],
    [Some("2 days 12:00:00.25"), Some("2 days 12:00:00.25"), Some("10 days 00:00:01")],
    [Some("5 days 00:00:00.932"), Some("5 days 00:00:00.932"), Some("4 days 24:00:00.068")],
    [
        Some("15 days 16:59:28.450795"),
        Some("15 days 16:59:28.450795"),
        Some("1 day 14:11:50.146136"),
    ],
    [Some("00:00:00.0432"), Some("00:00:00.0432"), Some("50000000 days 1388:53:20")],
    [None, None, Some("00:00:00.000432")],
    [Some("00:00:43.20005"), Some("00:00:43.20005"), Some("50000 days 01:23:20")],
    [Some("432002 days 24:00:00.25"), Some("432002 days 24:00:00.25"), Some("00:00:04.999977")],
    [
        Some("-3 days -18:00:00.375"),
        Some("-3 days -18:00:00.375"),
        Some("-6 days -16:00:00.666667"),
    ],
    [Some("00:00:00.000108"), Some("00:00:00.000108"), None],
    [Some("60 days 00:00:06"), Some("60 days 00:00:06"), Some("10:00:00.041667")],
    [Some("150 days 00:00:15"), Some("150 days 00:00:15"), Some("04:00:00.016667")],
    [Some("-5 days -00:00:00.5"), Some("-5 days -00:00:00.5"), Some("-5 days -00:00:00.5")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("-3 mons -7 days -00:00:00.000001"),
        Some("-3 mons -7 days -00:00:00.000001"),
        Some("-3 mons -7 days -00:00:00.000001"),
    ],
    [
        Some("-6 mons -14 days -00:00:00.000002"),
        Some("-6 mons -14 days -00:00:00.000002"),
        Some("-1 mons -18 days -12:00:00"),
    ],
    [
        Some("9 mons 21 days 00:00:00.000003"),
        Some("9 mons 21 days 00:00:00.000003"),
        Some("1 mon 2 days 08:00:00"),
    ],
    [
        Some("-1 years -9 mons -49 days -00:00:00.000007"),
        Some("-1 years -9 mons -49 days -00:00:00.000007"),
        Some("-13 days -20:34:17.1552"),
    ],
    [
        Some("-250000 years -7000000 days -00:00:01"),
        Some("-250000 years -7000000 days -00:00:01"),
        Some("-00:00:08.3808"),
    ],
    [None, None, Some("-00:00:00.000282")],
    [None, None, Some("00:00:00.000282")],
    [
        Some("-4 mons -25 days -12:00:00.000002"),
        Some("-4 mons -25 days -12:00:00.000002"),
        Some("-2 mons -4 days -16:00:00.000001"),
    ],
    [
        Some("-9 days -16:48:00"),
        Some("-9 days -16:48:00"),
        Some("-2 years -6 mons -70 days -00:00:00.00001"),
    ],
    [
        Some("7 mons 32 days 12:00:00.000002"),
        Some("7 mons 32 days 12:00:00.000002"),
        Some("1 mon 8 days 19:12:00"),
    ],
    [
        Some("-1 mons -2 days -08:00:00"),
        Some("-1 mons -2 days -08:00:00"),
        Some("-9 mons -21 days -00:00:00.000003"),
    ],
    [
        Some("-1 mons -18 days -12:00:00"),
        Some("-1 mons -18 days -12:00:00"),
        Some("-6 mons -14 days -00:00:00.000002"),
    ],
    [
        Some("-3 mons -7 days -00:00:08.380801"),
        Some("-3 mons -7 days -00:00:08.380801"),
        Some("-2 mons -36 days -23:59:51.619202"),
    ],
    [
        Some("-9 mons -34 days -17:37:17.472003"),
        Some("-9 mons -34 days -17:37:17.472003"),
        Some("-30 days -21:01:33.751374"),
    ],
    [
        Some("-00:00:00.83808"),
        Some("-00:00:00.83808"),
        Some("-2500000 years -70000000 days -00:00:10"),
    ],
    [None, None, Some("-00:00:00.000605")],
    [Some("-00:13:58.08"), Some("-00:13:58.08"), Some("-2500 years -70000 days -00:00:00.01")],
    [
        Some("-21600 years -1 mons -604818 days -12:00:00.0864"),
        Some("-21600 years -1 mons -604818 days -12:00:00.0864"),
        Some("-00:01:37.028759"),
    ],
    [
        Some("2 mons 12 days 18:00:00.000001"),
        Some("2 mons 12 days 18:00:00.000001"),
        Some("4 mons 9 days 08:00:00.000001"),
    ],
    [Some("-00:00:00.000151"), Some("-00:00:00.000151"), None],
    [
        Some("-3 years -84 days -00:00:00.000012"),
        Some("-3 years -84 days -00:00:00.000012"),
        Some("-8 days -02:00:00"),
    ],
    [
        Some("-7 years -6 mons -210 days -00:00:00.00003"),
        Some("-7 years -6 mons -210 days -00:00:00.00003"),
        Some("-3 days -05:36:00"),
    ],
    [
        Some("3 mons 7 days 00:00:00.000001"),
        Some("3 mons 7 days 00:00:00.000001"),
        Some("3 mons 7 days 00:00:00.000001"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("1 mon 30 days -23:59:59.999999"),
        Some("1 mon 30 days -23:59:59.999999"),
        Some("1 mon 30 days -23:59:59.999999"),
    ],
    [
        Some("2 mons 60 days -47:59:59.999998"),
        Some("2 mons 60 days -47:59:59.999998"),
        Some("30 days -12:00:00"),
    ],
    [
        Some("-3 mons -90 days +71:59:59.999997"),
        Some("-3 mons -90 days +71:59:59.999997"),
        Some("-20 days +08:00:00"),
    ],
    [
        Some("7 mons 210 days -167:59:59.999993"),
        Some("7 mons 210 days -167:59:59.999993"),
        Some("8 days 10:17:08.546743"),
    ],
    [
        Some("83333 years 4 mons 30000000 days -23999999:59:59"),
        Some("83333 years 4 mons 30000000 days -23999999:59:59"),
        Some("00:00:05.0976"),
    ],
    [None, None, Some("00:00:00.001167")],
    [None, None, Some("-00:00:00.001167")],
    [
        Some("1 mon 60 days -35:59:59.999998"),
        Some("1 mon 60 days -35:59:59.999998"),
        Some("40 days -15:59:59.999999"),
    ],
    [Some("6 days -02:24:00"), Some("6 days -02:24:00"), Some("10 mons 300 days -239:59:59.99999")],
    [
        Some("-2 mons -90 days +59:59:59.999998"),
        Some("-2 mons -90 days +59:59:59.999998"),
        Some("-24 days +09:36:00"),
    ],
    [Some("20 days -08:00:00"), Some("20 days -08:00:00"), Some("3 mons 90 days -71:59:59.999997")],
    [Some("30 days -12:00:00"), Some("30 days -12:00:00"), Some("2 mons 60 days -47:59:59.999998")],
    [
        Some("1 mon 30 days -23:59:54.902399"),
        Some("1 mon 30 days -23:59:54.902399"),
        Some("59 days -00:00:05.097596"),
    ],
    [
        Some("3 mons 98 days -63:30:30.815997"),
        Some("3 mons 98 days -63:30:30.815997"),
        Some("19 days -05:16:22.123509"),
    ],
    [
        Some("00:00:00.50976"),
        Some("00:00:00.50976"),
        Some("833333 years 4 mons 300000000 days -239999999:59:50"),
    ],
    [None, None, Some("00:00:00.002506")],
    [
        Some("00:08:29.76"),
        Some("00:08:29.76"),
        Some("833 years 4 mons 300000 days -239999:59:59.99"),
    ],
    [
        Some("7200 years 2592030 days -2073611:59:59.9136"),
        Some("7200 years 2592030 days -2073611:59:59.9136"),
        Some("00:00:58.980632"),
    ],
    [
        Some("-45 days +17:59:59.999999"),
        Some("-45 days +17:59:59.999999"),
        Some("-1 mons -50 days +31:59:59.999999"),
    ],
    [Some("00:00:00.000626"), Some("00:00:00.000626"), None],
    [
        Some("1 year 360 days -287:59:59.999988"),
        Some("1 year 360 days -287:59:59.999988"),
        Some("5 days -02:00:00"),
    ],
    [
        Some("2 years 6 mons 900 days -719:59:59.99997"),
        Some("2 years 6 mons 900 days -719:59:59.99997"),
        Some("2 days -00:48:00"),
    ],
    [
        Some("-1 mons -30 days +23:59:59.999999"),
        Some("-1 mons -30 days +23:59:59.999999"),
        Some("-1 mons -30 days +23:59:59.999999"),
    ],
    [Some("00:00:00"), Some("00:00:00"), None],
    [Some("2 mons 1 day"), Some("2 mons 1 day"), Some("2 mons 1 day")],
    [Some("4 mons 2 days"), Some("4 mons 2 days"), Some("1 mon 12:00:00")],
    [Some("-6 mons -3 days"), Some("-6 mons -3 days"), Some("-20 days -08:00:00")],
    [Some("1 year 2 mons 7 days"), Some("1 year 2 mons 7 days"), Some("8 days 17:08:34.322743")],
    [
        Some("166666 years 8 mons 1000000 days"),
        Some("166666 years 8 mons 1000000 days"),
        Some("00:00:05.2704"),
    ],
    [None, None, Some("00:00:00.00004")],
    [None, None, Some("-00:00:00.00004")],
    [Some("3 mons 1 day 12:00:00"), Some("3 mons 1 day 12:00:00"), Some("1 mon 10 days 16:00:00")],
    [Some("6 days 02:24:00"), Some("6 days 02:24:00"), Some("1 year 8 mons 10 days")],
    [
        Some("-5 mons -2 days -12:00:00"),
        Some("-5 mons -2 days -12:00:00"),
        Some("-24 days -09:36:00"),
    ],
    [Some("20 days 08:00:00"), Some("20 days 08:00:00"), Some("6 mons 3 days")],
    [Some("1 mon 12:00:00"), Some("1 mon 12:00:00"), Some("4 mons 2 days")],
    [
        Some("2 mons 1 day 00:00:05.2704"),
        Some("2 mons 1 day 00:00:05.2704"),
        Some("1 mon 30 days 23:59:54.7296"),
    ],
    [
        Some("6 mons 11 days 15:17:15.936"),
        Some("6 mons 11 days 15:17:15.936"),
        Some("19 days 10:00:21.814996"),
    ],
    [Some("00:00:00.52704"), Some("00:00:00.52704"), Some("1666666 years 8 mons 10000000 days")],
    [
        Some("166666666 years 8 mons 1000000000 days"),
        Some("166666666 years 8 mons 1000000000 days"),
        Some("00:00:00.000086"),
    ],
    [Some("00:08:47.04"), Some("00:08:47.04"), Some("1666 years 8 mons 10000 days")],
    [
        Some("14400 years 1 mon 86400 days 12:00:00"),
        Some("14400 years 1 mon 86400 days 12:00:00"),
        Some("00:01:00.961594"),
    ],
    [
        Some("-1 mons -15 days -18:00:00"),
        Some("-1 mons -15 days -18:00:00"),
        Some("-2 mons -21 days -08:00:00"),
    ],
    [Some("00:00:00.000022"), Some("00:00:00.000022"), None],
    [Some("2 years 12 days"), Some("2 years 12 days"), Some("5 days 02:00:00")],
    [Some("5 years 30 days"), Some("5 years 30 days"), Some("2 days 00:48:00")],
    [Some("-2 mons -1 days"), Some("-2 mons -1 days"), Some("-2 mons -1 days")],
    [Some("00:00:00"), Some("00:00:00"), None],
    [
        Some("100 years 100 days 100:00:00"),
        Some("100 years 100 days 100:00:00"),
        Some("100 years 100 days 100:00:00"),
    ],
    [
        Some("200 years 200 days 200:00:00"),
        Some("200 years 200 days 200:00:00"),
        Some("50 years 50 days 50:00:00"),
    ],
    [
        Some("-300 years -300 days -300:00:00"),
        Some("-300 years -300 days -300:00:00"),
        Some("-33 years -4 mons -33 days -41:20:00"),
    ],
    [
        Some("700 years 700 days 700:00:00"),
        Some("700 years 700 days 700:00:00"),
        Some("14 years 3 mons 27 days 17:42:51.440915"),
    ],
    [
        Some("100000000 years 100000000 days 100000000:00:00"),
        Some("100000000 years 100000000 days 100000000:00:00"),
        Some("00:51:59.4"),
    ],
    [None, None, Some("00:00:01.472991")],
    [None, None, Some("-00:00:01.472991")],
    [
        Some("150 years 150 days 150:00:00"),
        Some("150 years 150 days 150:00:00"),
        Some("66 years 8 mons 66 days 82:40:00"),
    ],
    [
        Some("10 years 10 days 10:00:00"),
        Some("10 years 10 days 10:00:00"),
        Some("1000 years 1000 days 1000:00:00"),
    ],
    [
        Some("-250 years -250 days -250:00:00"),
        Some("-250 years -250 days -250:00:00"),
        Some("-40 years -40 days -40:00:00"),
    ],
    [
        Some("33 years 4 mons 33 days 41:20:00"),
        Some("33 years 4 mons 33 days 41:20:00"),
        Some("300 years 300 days 300:00:00"),
    ],
    [
        Some("50 years 50 days 50:00:00"),
        Some("50 years 50 days 50:00:00"),
        Some("200 years 200 days 200:00:00"),
    ],
    [
        Some("100 years 100 days 100:51:59.4"),
        Some("100 years 100 days 100:51:59.4"),
        Some("99 years 11 mons 129 days 123:08:00.600009"),
    ],
    [
        Some("314 years 1 mon 341 days 323:44:06"),
        Some("314 years 1 mon 341 days 323:44:06"),
        Some("31 years 9 mons 60 days 55:44:57.68024"),
    ],
    [Some("00:05:11.94"), Some("00:05:11.94"), None],
    [None, None, Some("00:00:03.1194")],
    [
        Some("3 days 14:39:00"),
        Some("3 days 14:39:00"),
        Some("1000000 years 1000000 days 1000000:00:00"),
    ],
    [
        Some("8640050 years 8640050 days 8640050:00:00"),
        Some("8640050 years 8640050 days 8640050:00:00"),
        Some("10:01:43.935664"),
    ],
    [
        Some("-75 years -75 days -75:00:00"),
        Some("-75 years -75 days -75:00:00"),
        Some("-133 years -4 mons -133 days -141:20:00"),
    ],
    [Some("00:00:00.77985"), Some("00:00:00.77985"), None],
    [
        Some("1200 years 1200 days 1200:00:00"),
        Some("1200 years 1200 days 1200:00:00"),
        Some("8 years 4 mons 8 days 16:20:00"),
    ],
    [
        Some("3000 years 3000 days 3000:00:00"),
        Some("3000 years 3000 days 3000:00:00"),
        Some("3 years 4 mons 3 days 11:20:00"),
    ],
    [
        Some("-100 years -100 days -100:00:00"),
        Some("-100 years -100 days -100:00:00"),
        Some("-100 years -100 days -100:00:00"),
    ],
];

/// The intervals the shift corpus applies, as the statement writes them.
const SHIFT_INTERVALS: &[&str] = &[
    "1 day",
    "1 mon",
    "-1 mons",
    "1 year 2 mons 3 days 04:05:06.789",
    "1 mon -1 days +01:00:00",
    "-2 years +3 mons -00:00:00.5",
    "00:00:00",
    "23:59:59.999",
    "36:00:00",
    "-1 days -12:00:00",
    "12 mons 30 days",
    "10000 years",
    "-2025 years",
    "2 mons 1 day 00:00:01",
    "100:00:00.001",
];

/// A shift corpus row: a target type, its targets as the statement writes
/// them, and PostgreSQL's answers to `target + iv`, `target - iv` and
/// `iv + target` for each target and, within it, each interval of
/// [`SHIFT_INTERVALS`], `None` where PostgreSQL raises.
type ShiftRow<'a> = (&'a str, &'a [&'a str], &'a [[Option<&'a str>; 3]]);

/// The shift corpus.
const SHIFTS: &[ShiftRow<'static>] = &[
    (
        "date",
        &["2026-01-31", "2024-02-29", "0001-03-01", "9999-12-01"],
        &[
            [Some("2026-02-01 00:00:00"), Some("2026-01-30 00:00:00"), Some("2026-02-01 00:00:00")],
            [Some("2026-02-28 00:00:00"), Some("2025-12-31 00:00:00"), Some("2026-02-28 00:00:00")],
            [Some("2025-12-31 00:00:00"), Some("2026-02-28 00:00:00"), Some("2025-12-31 00:00:00")],
            [
                Some("2027-04-03 04:05:06.789"),
                Some("2024-11-26 19:54:53.211"),
                Some("2027-04-03 04:05:06.789"),
            ],
            [Some("2026-02-27 01:00:00"), Some("2025-12-31 23:00:00"), Some("2026-02-27 01:00:00")],
            [
                Some("2024-04-29 23:59:59.5"),
                Some("2027-10-31 00:00:00.5"),
                Some("2024-04-29 23:59:59.5"),
            ],
            [Some("2026-01-31 00:00:00"), Some("2026-01-31 00:00:00"), Some("2026-01-31 00:00:00")],
            [
                Some("2026-01-31 23:59:59.999"),
                Some("2026-01-30 00:00:00.001"),
                Some("2026-01-31 23:59:59.999"),
            ],
            [Some("2026-02-01 12:00:00"), Some("2026-01-29 12:00:00"), Some("2026-02-01 12:00:00")],
            [Some("2026-01-29 12:00:00"), Some("2026-02-01 12:00:00"), Some("2026-01-29 12:00:00")],
            [Some("2027-03-02 00:00:00"), Some("2025-01-01 00:00:00"), Some("2027-03-02 00:00:00")],
            [Some("12026-01-31 00:00:00"), None, Some("12026-01-31 00:00:00")],
            [Some("0001-01-31 00:00:00"), Some("4051-01-31 00:00:00"), Some("0001-01-31 00:00:00")],
            [Some("2026-04-01 00:00:01"), Some("2025-11-28 23:59:59"), Some("2026-04-01 00:00:01")],
            [
                Some("2026-02-04 04:00:00.001"),
                Some("2026-01-26 19:59:59.999"),
                Some("2026-02-04 04:00:00.001"),
            ],
            [Some("2024-03-01 00:00:00"), Some("2024-02-28 00:00:00"), Some("2024-03-01 00:00:00")],
            [Some("2024-03-29 00:00:00"), Some("2024-01-29 00:00:00"), Some("2024-03-29 00:00:00")],
            [Some("2024-01-29 00:00:00"), Some("2024-03-29 00:00:00"), Some("2024-01-29 00:00:00")],
            [
                Some("2025-05-02 04:05:06.789"),
                Some("2022-12-25 19:54:53.211"),
                Some("2025-05-02 04:05:06.789"),
            ],
            [Some("2024-03-28 01:00:00"), Some("2024-01-29 23:00:00"), Some("2024-03-28 01:00:00")],
            [
                Some("2022-05-28 23:59:59.5"),
                Some("2025-11-29 00:00:00.5"),
                Some("2022-05-28 23:59:59.5"),
            ],
            [Some("2024-02-29 00:00:00"), Some("2024-02-29 00:00:00"), Some("2024-02-29 00:00:00")],
            [
                Some("2024-02-29 23:59:59.999"),
                Some("2024-02-28 00:00:00.001"),
                Some("2024-02-29 23:59:59.999"),
            ],
            [Some("2024-03-01 12:00:00"), Some("2024-02-27 12:00:00"), Some("2024-03-01 12:00:00")],
            [Some("2024-02-27 12:00:00"), Some("2024-03-01 12:00:00"), Some("2024-02-27 12:00:00")],
            [Some("2025-03-30 00:00:00"), Some("2023-01-29 00:00:00"), Some("2025-03-30 00:00:00")],
            [Some("12024-02-29 00:00:00"), None, Some("12024-02-29 00:00:00")],
            [
                Some("0002-02-28 00:00:00 BC"),
                Some("4049-02-28 00:00:00"),
                Some("0002-02-28 00:00:00 BC"),
            ],
            [Some("2024-04-30 00:00:01"), Some("2023-12-27 23:59:59"), Some("2024-04-30 00:00:01")],
            [
                Some("2024-03-04 04:00:00.001"),
                Some("2024-02-24 19:59:59.999"),
                Some("2024-03-04 04:00:00.001"),
            ],
            [Some("0001-03-02 00:00:00"), Some("0001-02-28 00:00:00"), Some("0001-03-02 00:00:00")],
            [Some("0001-04-01 00:00:00"), Some("0001-02-01 00:00:00"), Some("0001-04-01 00:00:00")],
            [Some("0001-02-01 00:00:00"), Some("0001-04-01 00:00:00"), Some("0001-02-01 00:00:00")],
            [
                Some("0002-05-04 04:05:06.789"),
                Some("0002-12-28 19:54:53.211 BC"),
                Some("0002-05-04 04:05:06.789"),
            ],
            [Some("0001-03-31 01:00:00"), Some("0001-02-01 23:00:00"), Some("0001-03-31 01:00:00")],
            [
                Some("0002-05-31 23:59:59.5 BC"),
                Some("0002-12-01 00:00:00.5"),
                Some("0002-05-31 23:59:59.5 BC"),
            ],
            [Some("0001-03-01 00:00:00"), Some("0001-03-01 00:00:00"), Some("0001-03-01 00:00:00")],
            [
                Some("0001-03-01 23:59:59.999"),
                Some("0001-02-28 00:00:00.001"),
                Some("0001-03-01 23:59:59.999"),
            ],
            [Some("0001-03-02 12:00:00"), Some("0001-02-27 12:00:00"), Some("0001-03-02 12:00:00")],
            [Some("0001-02-27 12:00:00"), Some("0001-03-02 12:00:00"), Some("0001-02-27 12:00:00")],
            [
                Some("0002-03-31 00:00:00"),
                Some("0001-01-31 00:00:00 BC"),
                Some("0002-03-31 00:00:00"),
            ],
            [Some("10001-03-01 00:00:00"), None, Some("10001-03-01 00:00:00")],
            [
                Some("2025-03-01 00:00:00 BC"),
                Some("2026-03-01 00:00:00"),
                Some("2025-03-01 00:00:00 BC"),
            ],
            [
                Some("0001-05-02 00:00:01"),
                Some("0001-12-30 23:59:59 BC"),
                Some("0001-05-02 00:00:01"),
            ],
            [
                Some("0001-03-05 04:00:00.001"),
                Some("0001-02-24 19:59:59.999"),
                Some("0001-03-05 04:00:00.001"),
            ],
            [Some("9999-12-02 00:00:00"), Some("9999-11-30 00:00:00"), Some("9999-12-02 00:00:00")],
            [
                Some("10000-01-01 00:00:00"),
                Some("9999-11-01 00:00:00"),
                Some("10000-01-01 00:00:00"),
            ],
            [
                Some("9999-11-01 00:00:00"),
                Some("10000-01-01 00:00:00"),
                Some("9999-11-01 00:00:00"),
            ],
            [
                Some("10001-02-04 04:05:06.789"),
                Some("9998-09-27 19:54:53.211"),
                Some("10001-02-04 04:05:06.789"),
            ],
            [Some("9999-12-31 01:00:00"), Some("9999-11-01 23:00:00"), Some("9999-12-31 01:00:00")],
            [
                Some("9998-02-28 23:59:59.5"),
                Some("10001-09-01 00:00:00.5"),
                Some("9998-02-28 23:59:59.5"),
            ],
            [Some("9999-12-01 00:00:00"), Some("9999-12-01 00:00:00"), Some("9999-12-01 00:00:00")],
            [
                Some("9999-12-01 23:59:59.999"),
                Some("9999-11-30 00:00:00.001"),
                Some("9999-12-01 23:59:59.999"),
            ],
            [Some("9999-12-02 12:00:00"), Some("9999-11-29 12:00:00"), Some("9999-12-02 12:00:00")],
            [Some("9999-11-29 12:00:00"), Some("9999-12-02 12:00:00"), Some("9999-11-29 12:00:00")],
            [
                Some("10000-12-31 00:00:00"),
                Some("9998-11-01 00:00:00"),
                Some("10000-12-31 00:00:00"),
            ],
            [
                Some("19999-12-01 00:00:00"),
                Some("0002-12-01 00:00:00 BC"),
                Some("19999-12-01 00:00:00"),
            ],
            [
                Some("7974-12-01 00:00:00"),
                Some("12024-12-01 00:00:00"),
                Some("7974-12-01 00:00:00"),
            ],
            [
                Some("10000-02-02 00:00:01"),
                Some("9999-09-29 23:59:59"),
                Some("10000-02-02 00:00:01"),
            ],
            [
                Some("9999-12-05 04:00:00.001"),
                Some("9999-11-26 19:59:59.999"),
                Some("9999-12-05 04:00:00.001"),
            ],
        ],
    ),
    (
        "timestamp",
        &["2026-01-31 10:20:30.5", "2024-02-29 23:59:59.999", "2000-01-01 00:00:00"],
        &[
            [
                Some("2026-02-01 10:20:30.5"),
                Some("2026-01-30 10:20:30.5"),
                Some("2026-02-01 10:20:30.5"),
            ],
            [
                Some("2026-02-28 10:20:30.5"),
                Some("2025-12-31 10:20:30.5"),
                Some("2026-02-28 10:20:30.5"),
            ],
            [
                Some("2025-12-31 10:20:30.5"),
                Some("2026-02-28 10:20:30.5"),
                Some("2025-12-31 10:20:30.5"),
            ],
            [
                Some("2027-04-03 14:25:37.289"),
                Some("2024-11-27 06:15:23.711"),
                Some("2027-04-03 14:25:37.289"),
            ],
            [
                Some("2026-02-27 11:20:30.5"),
                Some("2026-01-01 09:20:30.5"),
                Some("2026-02-27 11:20:30.5"),
            ],
            [Some("2024-04-30 10:20:30"), Some("2027-10-31 10:20:31"), Some("2024-04-30 10:20:30")],
            [
                Some("2026-01-31 10:20:30.5"),
                Some("2026-01-31 10:20:30.5"),
                Some("2026-01-31 10:20:30.5"),
            ],
            [
                Some("2026-02-01 10:20:30.499"),
                Some("2026-01-30 10:20:30.501"),
                Some("2026-02-01 10:20:30.499"),
            ],
            [
                Some("2026-02-01 22:20:30.5"),
                Some("2026-01-29 22:20:30.5"),
                Some("2026-02-01 22:20:30.5"),
            ],
            [
                Some("2026-01-29 22:20:30.5"),
                Some("2026-02-01 22:20:30.5"),
                Some("2026-01-29 22:20:30.5"),
            ],
            [
                Some("2027-03-02 10:20:30.5"),
                Some("2025-01-01 10:20:30.5"),
                Some("2027-03-02 10:20:30.5"),
            ],
            [Some("12026-01-31 10:20:30.5"), None, Some("12026-01-31 10:20:30.5")],
            [
                Some("0001-01-31 10:20:30.5"),
                Some("4051-01-31 10:20:30.5"),
                Some("0001-01-31 10:20:30.5"),
            ],
            [
                Some("2026-04-01 10:20:31.5"),
                Some("2025-11-29 10:20:29.5"),
                Some("2026-04-01 10:20:31.5"),
            ],
            [
                Some("2026-02-04 14:20:30.501"),
                Some("2026-01-27 06:20:30.499"),
                Some("2026-02-04 14:20:30.501"),
            ],
            [
                Some("2024-03-01 23:59:59.999"),
                Some("2024-02-28 23:59:59.999"),
                Some("2024-03-01 23:59:59.999"),
            ],
            [
                Some("2024-03-29 23:59:59.999"),
                Some("2024-01-29 23:59:59.999"),
                Some("2024-03-29 23:59:59.999"),
            ],
            [
                Some("2024-01-29 23:59:59.999"),
                Some("2024-03-29 23:59:59.999"),
                Some("2024-01-29 23:59:59.999"),
            ],
            [
                Some("2025-05-03 04:05:06.788"),
                Some("2022-12-26 19:54:53.21"),
                Some("2025-05-03 04:05:06.788"),
            ],
            [
                Some("2024-03-29 00:59:59.999"),
                Some("2024-01-30 22:59:59.999"),
                Some("2024-03-29 00:59:59.999"),
            ],
            [
                Some("2022-05-29 23:59:59.499"),
                Some("2025-11-30 00:00:00.499"),
                Some("2022-05-29 23:59:59.499"),
            ],
            [
                Some("2024-02-29 23:59:59.999"),
                Some("2024-02-29 23:59:59.999"),
                Some("2024-02-29 23:59:59.999"),
            ],
            [
                Some("2024-03-01 23:59:59.998"),
                Some("2024-02-29 00:00:00"),
                Some("2024-03-01 23:59:59.998"),
            ],
            [
                Some("2024-03-02 11:59:59.999"),
                Some("2024-02-28 11:59:59.999"),
                Some("2024-03-02 11:59:59.999"),
            ],
            [
                Some("2024-02-28 11:59:59.999"),
                Some("2024-03-02 11:59:59.999"),
                Some("2024-02-28 11:59:59.999"),
            ],
            [
                Some("2025-03-30 23:59:59.999"),
                Some("2023-01-29 23:59:59.999"),
                Some("2025-03-30 23:59:59.999"),
            ],
            [Some("12024-02-29 23:59:59.999"), None, Some("12024-02-29 23:59:59.999")],
            [
                Some("0002-02-28 23:59:59.999 BC"),
                Some("4049-02-28 23:59:59.999"),
                Some("0002-02-28 23:59:59.999 BC"),
            ],
            [
                Some("2024-05-01 00:00:00.999"),
                Some("2023-12-28 23:59:58.999"),
                Some("2024-05-01 00:00:00.999"),
            ],
            [
                Some("2024-03-05 04:00:00"),
                Some("2024-02-25 19:59:59.998"),
                Some("2024-03-05 04:00:00"),
            ],
            [Some("2000-01-02 00:00:00"), Some("1999-12-31 00:00:00"), Some("2000-01-02 00:00:00")],
            [Some("2000-02-01 00:00:00"), Some("1999-12-01 00:00:00"), Some("2000-02-01 00:00:00")],
            [Some("1999-12-01 00:00:00"), Some("2000-02-01 00:00:00"), Some("1999-12-01 00:00:00")],
            [
                Some("2001-03-04 04:05:06.789"),
                Some("1998-10-28 19:54:53.211"),
                Some("2001-03-04 04:05:06.789"),
            ],
            [Some("2000-01-31 01:00:00"), Some("1999-12-01 23:00:00"), Some("2000-01-31 01:00:00")],
            [
                Some("1998-03-31 23:59:59.5"),
                Some("2001-10-01 00:00:00.5"),
                Some("1998-03-31 23:59:59.5"),
            ],
            [Some("2000-01-01 00:00:00"), Some("2000-01-01 00:00:00"), Some("2000-01-01 00:00:00")],
            [
                Some("2000-01-01 23:59:59.999"),
                Some("1999-12-31 00:00:00.001"),
                Some("2000-01-01 23:59:59.999"),
            ],
            [Some("2000-01-02 12:00:00"), Some("1999-12-30 12:00:00"), Some("2000-01-02 12:00:00")],
            [Some("1999-12-30 12:00:00"), Some("2000-01-02 12:00:00"), Some("1999-12-30 12:00:00")],
            [Some("2001-01-31 00:00:00"), Some("1998-12-02 00:00:00"), Some("2001-01-31 00:00:00")],
            [Some("12000-01-01 00:00:00"), None, Some("12000-01-01 00:00:00")],
            [
                Some("0026-01-01 00:00:00 BC"),
                Some("4025-01-01 00:00:00"),
                Some("0026-01-01 00:00:00 BC"),
            ],
            [Some("2000-03-02 00:00:01"), Some("1999-10-30 23:59:59"), Some("2000-03-02 00:00:01")],
            [
                Some("2000-01-05 04:00:00.001"),
                Some("1999-12-27 19:59:59.999"),
                Some("2000-01-05 04:00:00.001"),
            ],
        ],
    ),
    (
        "timestamptz",
        &["2026-01-31 10:20:30.5+00", "2024-02-29 23:00:00+00"],
        &[
            [
                Some("2026-02-01 10:20:30.5+00"),
                Some("2026-01-30 10:20:30.5+00"),
                Some("2026-02-01 10:20:30.5+00"),
            ],
            [
                Some("2026-02-28 10:20:30.5+00"),
                Some("2025-12-31 10:20:30.5+00"),
                Some("2026-02-28 10:20:30.5+00"),
            ],
            [
                Some("2025-12-31 10:20:30.5+00"),
                Some("2026-02-28 10:20:30.5+00"),
                Some("2025-12-31 10:20:30.5+00"),
            ],
            [
                Some("2027-04-03 14:25:37.289+00"),
                Some("2024-11-27 06:15:23.711+00"),
                Some("2027-04-03 14:25:37.289+00"),
            ],
            [
                Some("2026-02-27 11:20:30.5+00"),
                Some("2026-01-01 09:20:30.5+00"),
                Some("2026-02-27 11:20:30.5+00"),
            ],
            [
                Some("2024-04-30 10:20:30+00"),
                Some("2027-10-31 10:20:31+00"),
                Some("2024-04-30 10:20:30+00"),
            ],
            [
                Some("2026-01-31 10:20:30.5+00"),
                Some("2026-01-31 10:20:30.5+00"),
                Some("2026-01-31 10:20:30.5+00"),
            ],
            [
                Some("2026-02-01 10:20:30.499+00"),
                Some("2026-01-30 10:20:30.501+00"),
                Some("2026-02-01 10:20:30.499+00"),
            ],
            [
                Some("2026-02-01 22:20:30.5+00"),
                Some("2026-01-29 22:20:30.5+00"),
                Some("2026-02-01 22:20:30.5+00"),
            ],
            [
                Some("2026-01-29 22:20:30.5+00"),
                Some("2026-02-01 22:20:30.5+00"),
                Some("2026-01-29 22:20:30.5+00"),
            ],
            [
                Some("2027-03-02 10:20:30.5+00"),
                Some("2025-01-01 10:20:30.5+00"),
                Some("2027-03-02 10:20:30.5+00"),
            ],
            [Some("12026-01-31 10:20:30.5+00"), None, Some("12026-01-31 10:20:30.5+00")],
            [
                Some("0001-01-31 10:20:30.5+00"),
                Some("4051-01-31 10:20:30.5+00"),
                Some("0001-01-31 10:20:30.5+00"),
            ],
            [
                Some("2026-04-01 10:20:31.5+00"),
                Some("2025-11-29 10:20:29.5+00"),
                Some("2026-04-01 10:20:31.5+00"),
            ],
            [
                Some("2026-02-04 14:20:30.501+00"),
                Some("2026-01-27 06:20:30.499+00"),
                Some("2026-02-04 14:20:30.501+00"),
            ],
            [
                Some("2024-03-01 23:00:00+00"),
                Some("2024-02-28 23:00:00+00"),
                Some("2024-03-01 23:00:00+00"),
            ],
            [
                Some("2024-03-29 23:00:00+00"),
                Some("2024-01-29 23:00:00+00"),
                Some("2024-03-29 23:00:00+00"),
            ],
            [
                Some("2024-01-29 23:00:00+00"),
                Some("2024-03-29 23:00:00+00"),
                Some("2024-01-29 23:00:00+00"),
            ],
            [
                Some("2025-05-03 03:05:06.789+00"),
                Some("2022-12-26 18:54:53.211+00"),
                Some("2025-05-03 03:05:06.789+00"),
            ],
            [
                Some("2024-03-29 00:00:00+00"),
                Some("2024-01-30 22:00:00+00"),
                Some("2024-03-29 00:00:00+00"),
            ],
            [
                Some("2022-05-29 22:59:59.5+00"),
                Some("2025-11-29 23:00:00.5+00"),
                Some("2022-05-29 22:59:59.5+00"),
            ],
            [
                Some("2024-02-29 23:00:00+00"),
                Some("2024-02-29 23:00:00+00"),
                Some("2024-02-29 23:00:00+00"),
            ],
            [
                Some("2024-03-01 22:59:59.999+00"),
                Some("2024-02-28 23:00:00.001+00"),
                Some("2024-03-01 22:59:59.999+00"),
            ],
            [
                Some("2024-03-02 11:00:00+00"),
                Some("2024-02-28 11:00:00+00"),
                Some("2024-03-02 11:00:00+00"),
            ],
            [
                Some("2024-02-28 11:00:00+00"),
                Some("2024-03-02 11:00:00+00"),
                Some("2024-02-28 11:00:00+00"),
            ],
            [
                Some("2025-03-30 23:00:00+00"),
                Some("2023-01-29 23:00:00+00"),
                Some("2025-03-30 23:00:00+00"),
            ],
            [Some("12024-02-29 23:00:00+00"), None, Some("12024-02-29 23:00:00+00")],
            [
                Some("0002-02-28 23:00:00+00 BC"),
                Some("4049-02-28 23:00:00+00"),
                Some("0002-02-28 23:00:00+00 BC"),
            ],
            [
                Some("2024-04-30 23:00:01+00"),
                Some("2023-12-28 22:59:59+00"),
                Some("2024-04-30 23:00:01+00"),
            ],
            [
                Some("2024-03-05 03:00:00.001+00"),
                Some("2024-02-25 18:59:59.999+00"),
                Some("2024-03-05 03:00:00.001+00"),
            ],
        ],
    ),
    (
        "time",
        &["10:20:30.5", "23:59:59.999", "00:00:00"],
        &[
            [Some("10:20:30.5"), Some("10:20:30.5"), Some("10:20:30.5")],
            [Some("10:20:30.5"), Some("10:20:30.5"), Some("10:20:30.5")],
            [Some("10:20:30.5"), Some("10:20:30.5"), Some("10:20:30.5")],
            [Some("14:25:37.289"), Some("06:15:23.711"), Some("14:25:37.289")],
            [Some("11:20:30.5"), Some("09:20:30.5"), Some("11:20:30.5")],
            [Some("10:20:30"), Some("10:20:31"), Some("10:20:30")],
            [Some("10:20:30.5"), Some("10:20:30.5"), Some("10:20:30.5")],
            [Some("10:20:30.499"), Some("10:20:30.501"), Some("10:20:30.499")],
            [Some("22:20:30.5"), Some("22:20:30.5"), Some("22:20:30.5")],
            [Some("22:20:30.5"), Some("22:20:30.5"), Some("22:20:30.5")],
            [Some("10:20:30.5"), Some("10:20:30.5"), Some("10:20:30.5")],
            [Some("10:20:30.5"), Some("10:20:30.5"), Some("10:20:30.5")],
            [Some("10:20:30.5"), Some("10:20:30.5"), Some("10:20:30.5")],
            [Some("10:20:31.5"), Some("10:20:29.5"), Some("10:20:31.5")],
            [Some("14:20:30.501"), Some("06:20:30.499"), Some("14:20:30.501")],
            [Some("23:59:59.999"), Some("23:59:59.999"), Some("23:59:59.999")],
            [Some("23:59:59.999"), Some("23:59:59.999"), Some("23:59:59.999")],
            [Some("23:59:59.999"), Some("23:59:59.999"), Some("23:59:59.999")],
            [Some("04:05:06.788"), Some("19:54:53.21"), Some("04:05:06.788")],
            [Some("00:59:59.999"), Some("22:59:59.999"), Some("00:59:59.999")],
            [Some("23:59:59.499"), Some("00:00:00.499"), Some("23:59:59.499")],
            [Some("23:59:59.999"), Some("23:59:59.999"), Some("23:59:59.999")],
            [Some("23:59:59.998"), Some("00:00:00"), Some("23:59:59.998")],
            [Some("11:59:59.999"), Some("11:59:59.999"), Some("11:59:59.999")],
            [Some("11:59:59.999"), Some("11:59:59.999"), Some("11:59:59.999")],
            [Some("23:59:59.999"), Some("23:59:59.999"), Some("23:59:59.999")],
            [Some("23:59:59.999"), Some("23:59:59.999"), Some("23:59:59.999")],
            [Some("23:59:59.999"), Some("23:59:59.999"), Some("23:59:59.999")],
            [Some("00:00:00.999"), Some("23:59:58.999"), Some("00:00:00.999")],
            [Some("04:00:00"), Some("19:59:59.998"), Some("04:00:00")],
            [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
            [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
            [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
            [Some("04:05:06.789"), Some("19:54:53.211"), Some("04:05:06.789")],
            [Some("01:00:00"), Some("23:00:00"), Some("01:00:00")],
            [Some("23:59:59.5"), Some("00:00:00.5"), Some("23:59:59.5")],
            [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
            [Some("23:59:59.999"), Some("00:00:00.001"), Some("23:59:59.999")],
            [Some("12:00:00"), Some("12:00:00"), Some("12:00:00")],
            [Some("12:00:00"), Some("12:00:00"), Some("12:00:00")],
            [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
            [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
            [Some("00:00:00"), Some("00:00:00"), Some("00:00:00")],
            [Some("00:00:01"), Some("23:59:59"), Some("00:00:01")],
            [Some("04:00:00.001"), Some("19:59:59.999"), Some("04:00:00.001")],
        ],
    ),
    (
        "timetz",
        &["10:20:30.5+00", "23:00:00+05:30", "00:00:00-03"],
        &[
            [Some("10:20:30.5+00"), Some("10:20:30.5+00"), Some("10:20:30.5+00")],
            [Some("10:20:30.5+00"), Some("10:20:30.5+00"), Some("10:20:30.5+00")],
            [Some("10:20:30.5+00"), Some("10:20:30.5+00"), Some("10:20:30.5+00")],
            [Some("14:25:37.289+00"), Some("06:15:23.711+00"), Some("14:25:37.289+00")],
            [Some("11:20:30.5+00"), Some("09:20:30.5+00"), Some("11:20:30.5+00")],
            [Some("10:20:30+00"), Some("10:20:31+00"), Some("10:20:30+00")],
            [Some("10:20:30.5+00"), Some("10:20:30.5+00"), Some("10:20:30.5+00")],
            [Some("10:20:30.499+00"), Some("10:20:30.501+00"), Some("10:20:30.499+00")],
            [Some("22:20:30.5+00"), Some("22:20:30.5+00"), Some("22:20:30.5+00")],
            [Some("22:20:30.5+00"), Some("22:20:30.5+00"), Some("22:20:30.5+00")],
            [Some("10:20:30.5+00"), Some("10:20:30.5+00"), Some("10:20:30.5+00")],
            [Some("10:20:30.5+00"), Some("10:20:30.5+00"), Some("10:20:30.5+00")],
            [Some("10:20:30.5+00"), Some("10:20:30.5+00"), Some("10:20:30.5+00")],
            [Some("10:20:31.5+00"), Some("10:20:29.5+00"), Some("10:20:31.5+00")],
            [Some("14:20:30.501+00"), Some("06:20:30.499+00"), Some("14:20:30.501+00")],
            [Some("23:00:00+05:30"), Some("23:00:00+05:30"), Some("23:00:00+05:30")],
            [Some("23:00:00+05:30"), Some("23:00:00+05:30"), Some("23:00:00+05:30")],
            [Some("23:00:00+05:30"), Some("23:00:00+05:30"), Some("23:00:00+05:30")],
            [Some("03:05:06.789+05:30"), Some("18:54:53.211+05:30"), Some("03:05:06.789+05:30")],
            [Some("00:00:00+05:30"), Some("22:00:00+05:30"), Some("00:00:00+05:30")],
            [Some("22:59:59.5+05:30"), Some("23:00:00.5+05:30"), Some("22:59:59.5+05:30")],
            [Some("23:00:00+05:30"), Some("23:00:00+05:30"), Some("23:00:00+05:30")],
            [Some("22:59:59.999+05:30"), Some("23:00:00.001+05:30"), Some("22:59:59.999+05:30")],
            [Some("11:00:00+05:30"), Some("11:00:00+05:30"), Some("11:00:00+05:30")],
            [Some("11:00:00+05:30"), Some("11:00:00+05:30"), Some("11:00:00+05:30")],
            [Some("23:00:00+05:30"), Some("23:00:00+05:30"), Some("23:00:00+05:30")],
            [Some("23:00:00+05:30"), Some("23:00:00+05:30"), Some("23:00:00+05:30")],
            [Some("23:00:00+05:30"), Some("23:00:00+05:30"), Some("23:00:00+05:30")],
            [Some("23:00:01+05:30"), Some("22:59:59+05:30"), Some("23:00:01+05:30")],
            [Some("03:00:00.001+05:30"), Some("18:59:59.999+05:30"), Some("03:00:00.001+05:30")],
            [Some("00:00:00-03"), Some("00:00:00-03"), Some("00:00:00-03")],
            [Some("00:00:00-03"), Some("00:00:00-03"), Some("00:00:00-03")],
            [Some("00:00:00-03"), Some("00:00:00-03"), Some("00:00:00-03")],
            [Some("04:05:06.789-03"), Some("19:54:53.211-03"), Some("04:05:06.789-03")],
            [Some("01:00:00-03"), Some("23:00:00-03"), Some("01:00:00-03")],
            [Some("23:59:59.5-03"), Some("00:00:00.5-03"), Some("23:59:59.5-03")],
            [Some("00:00:00-03"), Some("00:00:00-03"), Some("00:00:00-03")],
            [Some("23:59:59.999-03"), Some("00:00:00.001-03"), Some("23:59:59.999-03")],
            [Some("12:00:00-03"), Some("12:00:00-03"), Some("12:00:00-03")],
            [Some("12:00:00-03"), Some("12:00:00-03"), Some("12:00:00-03")],
            [Some("00:00:00-03"), Some("00:00:00-03"), Some("00:00:00-03")],
            [Some("00:00:00-03"), Some("00:00:00-03"), Some("00:00:00-03")],
            [Some("00:00:00-03"), Some("00:00:00-03"), Some("00:00:00-03")],
            [Some("00:00:01-03"), Some("23:59:59-03"), Some("00:00:01-03")],
            [Some("04:00:00.001-03"), Some("19:59:59.999-03"), Some("04:00:00.001-03")],
        ],
    ),
];

const SCHEMA: &str = "
CREATE TABLE t (id int, iv interval, iv2 interval, ts timestamptz);
INSERT INTO t VALUES
    (1, '1 day', '24:00:00', '2026-01-01 00:00:00+00'),
    (2, '1 mon', '30 days', '2026-01-01 00:00:00+00'),
    (3, '10 days', '2 days', '2026-01-01 00:00:00+00');
CREATE TABLE n (id int, iv interval);
INSERT INTO n VALUES
    (-1, '-1 day'),
    (0, '00:00:00'),
    (1, '1 day'),
    (2, NULL),
    (3, '-1 days +02:00:00');
";

/// The differential pairs as one table.
fn pair_schema() -> String {
    let tuples: Vec<String> = PAIRS
        .iter()
        .enumerate()
        .map(|(index, (a, b, _, _, _))| format!("({}, '{a}', '{b}')", index + 1))
        .collect();
    format!(
        "CREATE TABLE piv (id int, a interval, b interval);\nINSERT INTO piv VALUES {};",
        tuples.join(", ")
    )
}

/// The sum corpus as one table.
fn sum_schema() -> String {
    let tuples: Vec<String> = SUM_ROWS
        .iter()
        .enumerate()
        .map(|(index, text)| format!("({}, '{text}')", index + 1))
        .collect();
    format!(
        "CREATE TABLE sumrows (id int, iv interval);\nINSERT INTO sumrows VALUES {};",
        tuples.join(", ")
    )
}

/// Asserts that translating `pg` is refused, and that the refusal names
/// `needle`.
fn refused(pg: &str, needle: &str) {
    assert_refused(&format!("{SCHEMA}\n{pg}"), needle);
}

#[test]
fn negating_the_corpus_matches_postgresql() {
    let schema = pair_schema();
    let got = run_translated_with(
        &format!("{schema}\nSELECT (-a)::text FROM piv ORDER BY id;"),
        &Pg2SqliteOptions::default(),
    );
    let want: Vec<Option<String>> =
        PAIRS.iter().map(|pair| pair.2.map(ToString::to_string)).collect();
    assert_eq!(got, want, "the negation of the corpus should match PostgreSQL");
}

#[test]
fn adding_the_corpus_matches_postgresql() {
    let schema = pair_schema();
    let got = run_translated_with(
        &format!("{schema}\nSELECT (a + b)::text FROM piv ORDER BY id;"),
        &Pg2SqliteOptions::default(),
    );
    let want: Vec<Option<String>> =
        PAIRS.iter().map(|pair| pair.3.map(ToString::to_string)).collect();
    assert_eq!(got, want, "the addition of the corpus should match PostgreSQL");
}

#[test]
fn subtracting_the_corpus_matches_postgresql() {
    let schema = pair_schema();
    let got = run_translated_with(
        &format!("{schema}\nSELECT (a - b)::text FROM piv ORDER BY id;"),
        &Pg2SqliteOptions::default(),
    );
    let want: Vec<Option<String>> =
        PAIRS.iter().map(|pair| pair.4.map(ToString::to_string)).collect();
    assert_eq!(got, want, "the subtraction of the corpus should match PostgreSQL");
}

#[test]
fn summing_the_corpus_matches_postgresql() {
    let schema = sum_schema();
    let got = run_translated_with(
        &format!("{schema}\nSELECT sum(iv)::text FROM sumrows;"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(got, vec![Some(SUM_TOTAL.to_string())]);
}

#[test]
fn summing_an_empty_set_answers_null() {
    let got = run_translated_default(&format!(
        "{SCHEMA}\nCREATE TABLE empty_iv (id int, iv interval);\nSELECT sum(iv)::text FROM empty_iv;"
    ));
    assert_eq!(got, vec![None]);
}

#[test]
fn summing_only_nulls_answers_null() {
    let got = run_translated_default(&format!(
        "{SCHEMA}\nCREATE TABLE null_iv (id int, iv interval);\nINSERT INTO null_iv VALUES (1, NULL);\nSELECT sum(iv)::text FROM null_iv;"
    ));
    assert_eq!(got, vec![None]);
}

#[test]
fn summing_a_total_that_leaves_range_answers_null() {
    let got = run_translated_default(&format!(
        "{SCHEMA}\nCREATE TABLE big_iv (id int, iv interval);\nINSERT INTO big_iv VALUES (1, '2147483647 mons'), (2, '1 mon');\nSELECT sum(iv)::text FROM big_iv;"
    ));
    assert_eq!(got, vec![None]);
}

#[test]
fn summing_a_total_postgresql_refuses_at_a_midway_step_answers_the_total() {
    // PostgreSQL errors when a running total leaves the 32-bit months range,
    // at its second row; the replica sums the fields, and the total fits.
    let got = run_translated_default(&format!(
        "{SCHEMA}\nCREATE TABLE mid_iv (id int, iv interval);\nINSERT INTO mid_iv VALUES (1, '2147483647 mons'), (2, '2147483647 mons'), (3, '-2147483648 mons');\nSELECT sum(iv)::text FROM mid_iv;"
    ));
    assert_eq!(got, vec![Some("178956970 years 6 mons".to_string())]);
}

#[test]
fn summing_micros_that_land_on_the_negative_bound_matches_postgresql() {
    let got = run_translated_default(&format!(
        "{SCHEMA}\nCREATE TABLE min_iv (id int, iv interval);\nINSERT INTO min_iv VALUES (1, '00:00:00'), (2, '-2562047788:00:54.775807'), (3, '-00:00:00.000001');\nSELECT sum(iv)::text FROM min_iv;"
    ));
    assert_eq!(got, vec![Some("-2562047788:00:54.775808".to_string())]);
}

#[test]
fn negating_a_null_answers_null() {
    let got = run_translated_default(&format!("{SCHEMA}\nSELECT (-iv)::text FROM n WHERE id = 2;"));
    assert_eq!(got, vec![None]);
}

#[test]
fn negating_matches_postgresql() {
    let got = run_translated_default(&format!("{SCHEMA}\nSELECT (-iv)::text FROM n ORDER BY id;"));
    assert_eq!(
        got,
        vec![
            Some("1 day".to_string()),
            Some("00:00:00".to_string()),
            Some("-1 days".to_string()),
            None,
            Some("1 day -02:00:00".to_string()),
        ]
    );
}

#[test]
fn an_interval_beside_null_answers_null() {
    for pg in [
        "SELECT (iv + NULL)::text FROM t WHERE id = 1;",
        "SELECT (NULL + iv)::text FROM t WHERE id = 1;",
        "SELECT (iv - NULL)::text FROM t WHERE id = 1;",
    ] {
        assert_eq!(
            run_translated_default(&format!("{SCHEMA}\n{pg}")),
            vec![None],
            "{pg} should answer NULL"
        );
    }
}

#[test]
fn an_interval_beside_a_string_literal_matches_postgresql() {
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (iv + '2 days')::text FROM t ORDER BY id;"
    ));
    assert_eq!(
        got,
        vec![
            Some("3 days".to_string()),
            Some("1 mon 2 days".to_string()),
            Some("12 days".to_string()),
        ]
    );
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (iv - '1 day')::text FROM t ORDER BY id;"
    ));
    assert_eq!(
        got,
        vec![
            Some("00:00:00".to_string()),
            Some("1 mon -1 days".to_string()),
            Some("9 days".to_string()),
        ]
    );
}

#[test]
fn an_interval_beside_a_typed_literal_matches_postgresql() {
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (iv + INTERVAL '1 day')::text FROM t ORDER BY id;"
    ));
    assert_eq!(
        got,
        vec![
            Some("2 days".to_string()),
            Some("1 mon 1 day".to_string()),
            Some("11 days".to_string()),
        ]
    );
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (iv - interval '2 hours')::text FROM t ORDER BY id;"
    ));
    assert_eq!(
        got,
        vec![
            Some("1 day -02:00:00".to_string()),
            Some("1 mon -02:00:00".to_string()),
            Some("10 days -02:00:00".to_string()),
        ]
    );
}

#[test]
fn literal_arithmetic_answers_postgresqls_literal() {
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (INTERVAL '1 day' + INTERVAL '2 hours')::text;"
    ));
    assert_eq!(got, vec![Some("1 day 02:00:00".to_string())]);
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (INTERVAL '1 day' - INTERVAL '2 hours')::text;"
    ));
    assert_eq!(got, vec![Some("1 day -02:00:00".to_string())]);
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT ((INTERVAL '1 day') + (INTERVAL '2 hours'))::text;"
    ));
    assert_eq!(got, vec![Some("1 day 02:00:00".to_string())]);
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (INTERVAL '1 day' + INTERVAL '2 hours' + INTERVAL '3 minutes')::text;"
    ));
    assert_eq!(got, vec![Some("1 day 02:03:00".to_string())]);
    let got = run_translated_default(&format!("{SCHEMA}\nSELECT (-INTERVAL '1 day')::text;"));
    assert_eq!(got, vec![Some("-1 days".to_string())]);
}

#[test]
fn a_string_literal_beside_a_typed_literal_matches_postgresql() {
    // PostgreSQL's unknown-literal rule resolves the string as an interval
    // beside the typed one, and the pair answers the literal arithmetic.
    let got =
        run_translated_default(&format!("{SCHEMA}\nSELECT ('2 days' + interval '1 day')::text;"));
    assert_eq!(got, vec![Some("3 days".to_string())]);
    let got =
        run_translated_default(&format!("{SCHEMA}\nSELECT (interval '1 day' - '2 days')::text;"));
    assert_eq!(got, vec![Some("-1 days".to_string())]);
}

#[test]
fn a_literal_answer_over_the_field_limits_is_refused() {
    refused(
        "SELECT (INTERVAL '2147483647 mons' + INTERVAL '1 mon')::text;",
        "interval out of range",
    );
    refused("SELECT (-interval '-2147483648 days')::text;", "interval out of range");
    refused(
        "SELECT (interval '2147483647 mons' - interval '-2147483647 mons')::text;",
        "interval out of range",
    );
}

#[test]
fn a_literal_sum_past_a_field_limit_is_refused_with_postgresqls_error() {
    // PostgreSQL answers every one of these with `interval out of range`:
    // the micros field overflows its 64-bit range, and the months and days
    // fields their 32-bit ranges, in both directions.
    refused(
        "SELECT (interval '2562047788:00:00' + interval '2562047788:00:00')::text;",
        "interval out of range",
    );
    refused(
        "SELECT (interval '-2562047788:00:00' - interval '2562047788:00:00')::text;",
        "interval out of range",
    );
    refused(
        "SELECT (interval '2147483647 days' + interval '2147483647 days')::text;",
        "interval out of range",
    );
    refused(
        "SELECT (interval '-2147483647 days' + interval '-2147483647 days')::text;",
        "interval out of range",
    );
    refused(
        "SELECT (interval '-2147483647 mons' + interval '-2147483647 mons')::text;",
        "interval out of range",
    );
}

#[test]
fn negating_a_null_interval_answers_null() {
    let got = run_translated_default(&format!("{SCHEMA}\nSELECT (-NULL::interval)::text;"));
    assert_eq!(got, vec![None]);
}

#[test]
fn a_literal_beside_a_column_interval_matches_postgresql() {
    // PostgreSQL adds the literal to each row's column interval; the replica
    // folds the literal's fields in and reads the column's at run time.
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (interval '1 day' + iv)::text FROM t ORDER BY id;"
    ));
    assert_eq!(
        got,
        vec![Some("2 days".into()), Some("1 mon 1 day".into()), Some("11 days".into())]
    );
}

#[test]
fn an_arithmetic_beside_a_null_interval_answers_null() {
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (iv + NULL::interval)::text FROM t ORDER BY id;"
    ));
    assert_eq!(got, vec![None, None, None]);
}

#[test]
fn a_literal_postgresql_refuses_to_read_is_refused() {
    refused(
        "SELECT (interval '2147483648 mons' + interval '1 day')::text;",
        "interval field value out of range",
    );
    refused(
        "SELECT (interval '1 day' + 'bad interval')::text;",
        "invalid input syntax for type interval",
    );
}

/// The answers as `Some` texts, for comparing with a run.
fn some(values: &[&str]) -> Vec<Option<String>> {
    values.iter().map(|value| Some((*value).to_string())).collect()
}

#[test]
fn multiplication_division_and_avg_match_postgresql() {
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (iv * 2)::text FROM t ORDER BY id;")),
        some(&["2 days", "2 mons", "20 days"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (2 * iv)::text FROM t ORDER BY id;")),
        some(&["2 days", "2 mons", "20 days"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (iv / 2)::text FROM t ORDER BY id;")),
        some(&["12:00:00", "15 days", "5 days"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT avg(iv)::text FROM t;")),
        some(&["13 days 16:00:00"])
    );
}

/// The scaled corpus intervals as one table.
fn scaled_schema() -> String {
    let tuples: Vec<String> = SCALED_INTERVALS
        .iter()
        .enumerate()
        .map(|(index, text)| format!("({}, '{text}')", index + 1))
        .collect();
    format!("CREATE TABLE sc (id int, iv interval);\nINSERT INTO sc VALUES {};", tuples.join(", "))
}

/// PostgreSQL's answer for the interval at `interval` and the factor at
/// `factor` under `op`: 0 for `iv * f`, 1 for `f * iv`, 2 for `iv / f`.
fn scaled_answer(interval: usize, factor: usize, op: usize) -> Option<String> {
    SCALED[interval * FACTORS.len() + factor][op].map(ToString::to_string)
}

/// The statements of the three operations for `interval` and `factor`.
fn scaled_expressions(interval: &str, factor: &str) -> [String; 3] {
    [
        format!("{interval} * {factor}"),
        format!("{factor} * {interval}"),
        format!("{interval} / {factor}"),
    ]
}

#[test]
fn scaling_an_interval_column_by_a_literal_matches_postgresql() {
    let schema = scaled_schema();
    for (factor_index, factor) in FACTORS.iter().enumerate() {
        for (op, expression) in scaled_expressions("iv", factor).iter().enumerate() {
            let got = run_translated_with(
                &format!("{schema}\nSELECT ({expression})::text FROM sc ORDER BY id;"),
                &Pg2SqliteOptions::default(),
            );
            let want: Vec<Option<String>> = (0..SCALED_INTERVALS.len())
                .map(|interval| scaled_answer(interval, factor_index, op))
                .collect();
            assert_eq!(got, want, "{expression} should match PostgreSQL");
        }
    }
}

#[test]
fn scaling_an_interval_literal_by_a_literal_matches_postgresql() {
    for (interval_index, interval) in SCALED_INTERVALS.iter().enumerate() {
        for (factor_index, factor) in FACTORS.iter().enumerate() {
            let literal = format!("interval '{interval}'");
            for (op, expression) in scaled_expressions(&literal, factor).iter().enumerate() {
                let pg = format!("SELECT ({expression})::text;");
                if let Some(answer) = scaled_answer(interval_index, factor_index, op) {
                    assert_eq!(
                        run_translated_with(&pg, &Pg2SqliteOptions::default()),
                        vec![Some(answer)],
                        "{expression} should match PostgreSQL"
                    );
                } else {
                    let error = Pg2Sqlite::default()
                        .sql(&pg)
                        .expect("parse")
                        .translate_to_sql(&Pg2SqliteOptions::default())
                        .expect_err("PostgreSQL raises, so the translation refuses");
                    let needle = if op == 2 && *factor == "0" {
                        "division by zero"
                    } else {
                        "interval out of range"
                    };
                    assert!(
                        error.to_string().contains(needle),
                        "{expression}: refusal {error} should name {needle}"
                    );
                }
            }
        }
    }
}

/// Scales every corpus interval by every factor `column` holds, the factors
/// at `factors` of [`FACTORS`] in that order, declared as `column_type`.
fn assert_column_factor_matches(column_type: &str, factors: &[usize]) {
    let tuples: Vec<String> = factors
        .iter()
        .enumerate()
        .map(|(position, factor)| format!("({}, {})", position + 1, FACTORS[*factor]))
        .collect();
    let schema = format!(
        "{}\nCREATE TABLE fa (id int, n {column_type});\nINSERT INTO fa VALUES {};",
        scaled_schema(),
        tuples.join(", ")
    );
    for (op, expression) in scaled_expressions("sc.iv", "fa.n").iter().enumerate() {
        let got = run_translated_with(
            &format!("{schema}\nSELECT ({expression})::text FROM sc, fa ORDER BY sc.id, fa.id;"),
            &Pg2SqliteOptions::default(),
        );
        let want: Vec<Option<String>> = (0..SCALED_INTERVALS.len())
            .flat_map(|interval| {
                factors.iter().map(move |factor| scaled_answer(interval, *factor, op))
            })
            .collect();
        assert_eq!(got, want, "{expression} over a {column_type} factor should match PostgreSQL");
    }
}

#[test]
fn scaling_an_interval_column_by_an_integer_column_matches_postgresql() {
    let integers: Vec<usize> = FACTORS
        .iter()
        .enumerate()
        .filter(|(_, factor)| factor.parse::<i64>().is_ok())
        .map(|(index, _)| index)
        .collect();
    assert_column_factor_matches("bigint", &integers);
}

#[test]
fn scaling_an_interval_column_by_a_numeric_column_matches_postgresql() {
    let fitting: Vec<usize> = FACTORS
        .iter()
        .enumerate()
        .filter(|(_, factor)| {
            !factor.contains('e') && factor.split('.').nth(1).is_none_or(|digits| digits.len() <= 6)
        })
        .map(|(index, _)| index)
        .collect();
    assert_column_factor_matches("numeric(18,6)", &fitting);
}

#[test]
fn scaling_by_a_bound_parameter_matches_postgresql() {
    let factor =
        FACTORS.iter().position(|factor| *factor == "1.5").expect("1.5 is a corpus factor");
    let got = run_translated_bound_with(
        &format!("{}\nSELECT (iv * $1)::text FROM sc ORDER BY id;", scaled_schema()),
        &Pg2SqliteOptions::default(),
        &["1.5"],
    );
    let want: Vec<Option<String>> =
        (0..SCALED_INTERVALS.len()).map(|interval| scaled_answer(interval, factor, 0)).collect();
    assert_eq!(got, want);
}

#[test]
fn a_factor_column_named_like_a_field_reads_its_own_value() {
    let schema = "CREATE TABLE cap (id int, iv interval, d int, m int, u int);\nINSERT INTO cap VALUES (1, '1 day 01:00:00', 3, 5, 7);";
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\n{schema}\nSELECT (iv * d)::text FROM cap;")),
        some(&["3 days 03:00:00"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\n{schema}\nSELECT (iv * m)::text FROM cap;")),
        some(&["5 days 05:00:00"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\n{schema}\nSELECT (iv / u)::text FROM cap;")),
        some(&["03:34:17.142857"])
    );
}

#[test]
fn scaling_by_null_answers_null() {
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (iv * NULL)::text FROM t ORDER BY id;")),
        vec![None, None, None]
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (iv / NULL)::text FROM t ORDER BY id;")),
        vec![None, None, None]
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (iv * 2)::text FROM n WHERE id = 2;")),
        vec![None]
    );
}

#[test]
fn scaled_intervals_compose_with_the_other_reads() {
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\nSELECT (iv * 2 + iv)::text FROM t ORDER BY id;"
        )),
        some(&["3 days", "3 mons", "30 days"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (-(iv / 3))::text FROM t ORDER BY id;")),
        some(&["-08:00:00", "-10 days", "-3 days -08:00:00"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT sum(iv * 2)::text FROM t;")),
        some(&["2 mons 22 days"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT id::text FROM t ORDER BY iv * 2;")),
        some(&["1", "3", "2"])
    );
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\nSELECT (iv * 1.5 < interval '3 days')::text FROM t ORDER BY id;"
        )),
        some(&["true", "false", "false"])
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (iv / 3 * 3)::text FROM t ORDER BY id;")),
        some(&["24:00:00", "30 days", "9 days 24:00:00"])
    );
}

#[test]
fn averaging_matches_postgresql() {
    let groups = "CREATE TABLE g (k int, iv interval);\nINSERT INTO g VALUES (1, '1 day'), (1, '2 days'), (1, NULL), (2, '1 mon'), (2, '-1 days +00:00:00.000001'), (3, NULL);";
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\n{groups}\nSELECT avg(iv)::text FROM g GROUP BY k ORDER BY k;"
        )),
        vec![Some("1 day 12:00:00".to_string()), Some("15 days -12:00:00".to_string()), None]
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT avg(iv)::text FROM t WHERE id > 3;")),
        vec![None]
    );
    for (rows, answer) in [
        ("('1 mon'), ('1 day'), ('00:00:01')", "10 days 08:00:00.333333"),
        (
            "('1 year'), ('1 mon'), ('1 day'), ('1 hour'), ('1 minute'), ('1 second'), ('00:00:00.000001')",
            "1 mon 25 days 20:43:00.167543",
        ),
        (
            "('178956970 years 7 mons'), ('-1 mon'), ('1 mon')",
            "59652323 years 6 mons 10 days 00:00:00.0864",
        ),
    ] {
        let values = rows.replace("('", "(1, '");
        assert_eq!(
            run_translated_default(&format!(
                "{SCHEMA}\nCREATE TABLE a (id int, iv interval);\nINSERT INTO a VALUES {values};\nSELECT avg(iv)::text FROM a;"
            )),
            some(&[answer]),
            "avg over {rows}"
        );
    }
}

#[test]
fn an_average_whose_total_leaves_range_answers_null() {
    // PostgreSQL raises `interval out of range` while accumulating; the
    // replica answers NULL, as the README's Quiet answers say.
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\nCREATE TABLE a (id int, iv interval);\nINSERT INTO a VALUES (1, '178956970 years 7 mons'), (2, '1 mon');\nSELECT avg(iv)::text FROM a;"
        )),
        vec![None]
    );
}

#[test]
fn scaling_refuses_what_postgresql_refuses() {
    refused(
        "SELECT (interval '1 day' * interval '1 day')::text;",
        "operator does not exist: interval * interval",
    );
    refused("SELECT (2 / iv)::text FROM t;", "operator does not exist: integer / interval");
    refused("SELECT (iv / iv2)::text FROM t;", "operator does not exist: interval / interval");
    refused(
        "CREATE TABLE x (iv interval, s text);\nSELECT (iv * s)::text FROM x;",
        "operator does not exist: interval * text",
    );
    refused(
        "SELECT (interval '1 day' * 1e400)::text;",
        "is out of range for type double precision",
    );
    refused("SELECT (interval '1 day' / 0)::text;", "division by zero");
    refused("SELECT (interval '1 day' / 0.0)::text;", "division by zero");
    refused("SELECT (interval '2147483647 days' * 2)::text;", "interval out of range");
}

#[test]
fn a_factor_without_an_exact_reading_is_refused() {
    refused(
        "CREATE TABLE x (iv interval, dp double precision);\nSELECT (iv * dp)::text FROM x;",
        "cannot be translated exactly",
    );
    refused("SELECT (iv * (id + 1))::text FROM t;", "cannot be translated exactly");
    refused("SELECT ((id + 1) * iv)::text FROM t;", "cannot be translated exactly");
    refused(
        "SELECT (interval '1 day' * CAST(NULL AS text))::text;",
        "operator does not exist: interval * text",
    );
}

#[test]
fn a_type_postgresql_has_no_operator_for_is_named_as_postgresql_names_it() {
    let schema = "CREATE TABLE ty (iv interval, a int2, b int8, c float4, d float8, e decimal(10,2), f varchar(10), g timestamp, h timetz);";
    for (expression, message) in [
        ("iv + a", "operator does not exist: interval + smallint"),
        ("iv + b", "operator does not exist: interval + bigint"),
        ("iv + c", "operator does not exist: interval + real"),
        ("iv + d", "operator does not exist: interval + double precision"),
        ("iv + e", "operator does not exist: interval + numeric"),
        ("iv + f", "operator does not exist: interval + character varying"),
        ("iv - g", "operator does not exist: interval - timestamp without time zone"),
        ("iv - h", "operator does not exist: interval - time with time zone"),
    ] {
        refused(&format!("{schema}\nSELECT ({expression})::text FROM ty;"), message);
    }
}

#[test]
fn a_zero_factor_column_answers_null_for_division() {
    // PostgreSQL raises `division by zero`; the replica answers NULL.
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\nCREATE TABLE z (n int);\nINSERT INTO z VALUES (0);\nSELECT (t.iv / z.n)::text FROM t, z ORDER BY t.id;"
        )),
        vec![None, None, None]
    );
}

#[test]
fn averaging_refuses_the_clauses_it_cannot_carry() {
    refused("SELECT avg(DISTINCT iv)::text FROM t;", "avg(DISTINCT) over an interval");
    refused("SELECT (avg(iv) OVER ())::text FROM t;", "avg over an interval with an OVER clause");
    refused(
        "SELECT (avg(iv) FILTER (WHERE id > 1))::text FROM t;",
        "avg over an interval with a FILTER",
    );
    refused("SELECT avg(iv, iv2)::text FROM t;", "function avg(interval, interval) does not exist");
    refused(
        "SELECT avg(CASE WHEN random() > 0.5 THEN iv ELSE iv2 END)::text FROM t;",
        "an interval average has no SQLite form",
    );
}

#[test]
fn scaling_in_a_generated_column_or_index_is_refused() {
    refused("CREATE INDEX ON t (iv * 2);", "scalar subquery");
    refused(
        "CREATE TABLE gen_m (iv interval, g interval GENERATED ALWAYS AS (iv * 2) STORED);",
        "is not legal in that position",
    );
}

#[test]
fn an_interval_beside_a_number_is_refused_with_postgresqls_message() {
    refused("SELECT (iv + 5)::text FROM t;", "operator does not exist: interval + integer");
    refused("SELECT (iv - 5)::text FROM t;", "operator does not exist: interval - integer");
}

/// Whether a date or timestamp answer PostgreSQL prints falls in the years 1
/// to 9999 the replica holds.
fn in_replica_years(answer: &str) -> bool {
    answer.as_bytes().get(4) == Some(&b'-')
        && answer[..4].bytes().all(|byte| byte.is_ascii_digit())
        && !answer.starts_with("0000")
        && !answer.ends_with(" BC")
}

/// The shift corpus rows of `kind` as one table of targets and intervals,
/// in target-major order.
fn shift_schema(kind: &str, targets: &[&str]) -> String {
    let tuples: Vec<String> = targets
        .iter()
        .flat_map(|target| SHIFT_INTERVALS.iter().map(move |interval| (target, interval)))
        .enumerate()
        .map(|(index, (target, interval))| format!("({}, '{target}', '{interval}')", index + 1))
        .collect();
    format!(
        "CREATE TABLE sh (id int, tg {kind}, iv interval);\nINSERT INTO sh VALUES {};",
        tuples.join(", ")
    )
}

/// The three shift statements over `target` and `interval`.
fn shift_expressions(target: &str, interval: &str) -> [String; 3] {
    [
        format!("{target} + {interval}"),
        format!("{target} - {interval}"),
        format!("{interval} + {target}"),
    ]
}

/// The corpus shift whose month step passes the year 9999 before its
/// negative day step brings it back: PostgreSQL answers inside the years,
/// and SQLite's date functions answer `NULL` at the month step, as for a
/// literal interval.
const PAST_9999_MIDWAY: (&str, &str) = ("9999-12-01", "1 mon -1 days +01:00:00");

#[test]
fn shifting_by_an_interval_column_matches_postgresql() {
    for (kind, targets, answers) in
        SHIFTS.iter().filter(|(kind, _, _)| matches!(*kind, "date" | "timestamp" | "time"))
    {
        let schema = shift_schema(kind, targets);
        for (op, expression) in shift_expressions("tg", "iv").iter().enumerate() {
            let got = run_translated_with(
                &format!("{schema}\nSELECT ({expression})::text FROM sh ORDER BY id;"),
                &Pg2SqliteOptions::default(),
            );
            let want: Vec<Option<String>> = answers
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let target = targets[index / SHIFT_INTERVALS.len()];
                    let interval = SHIFT_INTERVALS[index % SHIFT_INTERVALS.len()];
                    if (target, interval) == PAST_9999_MIDWAY && op != 1 {
                        return None;
                    }
                    row[op]
                        .filter(|answer| *kind == "time" || in_replica_years(answer))
                        .map(ToString::to_string)
                })
                .collect();
            assert_eq!(got, want, "{expression} over {kind} should match PostgreSQL");
        }
    }
}

#[test]
fn a_shift_passing_the_year_9999_midway_answers_null() {
    let (target, interval) = PAST_9999_MIDWAY;
    let index = SHIFT_INTERVALS.iter().position(|text| *text == interval).expect("corpus interval");
    let (_, targets, answers) =
        SHIFTS.iter().find(|(kind, _, _)| *kind == "date").expect("date corpus");
    let row = targets.iter().position(|text| *text == target).expect("corpus target");
    assert_eq!(answers[row * SHIFT_INTERVALS.len() + index][0], Some("9999-12-31 01:00:00"));
    for pg in [
        format!(
            "CREATE TABLE p (d date, iv interval);\nINSERT INTO p VALUES ('{target}', '{interval}');\nSELECT (d + iv)::text FROM p;"
        ),
        format!(
            "CREATE TABLE p (d date);\nINSERT INTO p VALUES ('{target}');\nSELECT (d + INTERVAL '{interval}')::text FROM p;"
        ),
    ] {
        assert_eq!(run_translated_with(&pg, &Pg2SqliteOptions::default()), vec![None], "{pg}");
    }
}

#[test]
fn shifting_by_an_interval_column_answers_what_the_literal_shift_answers() {
    for (kind, targets, _) in SHIFTS {
        let schema = shift_schema(kind, targets);
        for (op, expression) in shift_expressions("tg", "iv").iter().enumerate() {
            let got = run_translated_with(
                &format!("{schema}\nSELECT ({expression})::text FROM sh ORDER BY id;"),
                &Pg2SqliteOptions::default(),
            );
            for (row, value) in got.iter().enumerate() {
                let interval = SHIFT_INTERVALS[row % SHIFT_INTERVALS.len()];
                let literal = &shift_expressions("tg", &format!("INTERVAL '{interval}'"))[op];
                let script = Pg2Sqlite::default()
                    .sql(&format!(
                        "{schema}\nSELECT ({literal})::text FROM sh WHERE id = {};",
                        row + 1
                    ))
                    .expect("parse")
                    .translate_to_sql(&Pg2SqliteOptions::default());
                let Ok(script) = script else {
                    // The literal path refuses what it can decide is out of
                    // range; the column form answers NULL there.
                    assert_eq!(*value, None, "{literal} over {kind} row {}", row + 1);
                    continue;
                };
                drop(script);
                let literal_answer = run_translated_with(
                    &format!("{schema}\nSELECT ({literal})::text FROM sh WHERE id = {};", row + 1),
                    &Pg2SqliteOptions::default(),
                );
                assert_eq!(
                    vec![value.clone()],
                    literal_answer,
                    "{expression} and {literal} over {kind} row {}",
                    row + 1
                );
            }
        }
    }
}

#[test]
fn a_shift_beside_an_interval_column_matches_postgresql() {
    let schema = "CREATE TABLE sh2 (id int, iv interval, d date, tm time, ts timestamp);\nINSERT INTO sh2 VALUES (1, '1 day', '2026-01-01', '00:00:00', '2026-01-01 00:00:00'), (2, '1 mon -1 days +01:00:00', '2026-01-31', '23:30:00', '2026-01-31 23:30:00');";
    for (pg, answers) in [
        (
            "SELECT (d + iv)::text FROM sh2 ORDER BY id;",
            ["2026-01-02 00:00:00", "2026-02-27 01:00:00"],
        ),
        ("SELECT (tm + iv)::text FROM sh2 ORDER BY id;", ["00:00:00", "00:30:00"]),
        (
            "SELECT (ts - iv)::text FROM sh2 ORDER BY id;",
            ["2025-12-31 00:00:00", "2026-01-01 22:30:00"],
        ),
        (
            "SELECT (iv + ts)::text FROM sh2 ORDER BY id;",
            ["2026-01-02 00:00:00", "2026-02-28 00:30:00"],
        ),
    ] {
        assert_eq!(
            run_translated_default(&format!("{SCHEMA}\n{schema}\n{pg}")),
            some(&answers),
            "{pg}"
        );
    }
}

#[test]
fn a_time_shifted_by_a_literal_ignores_the_months_and_days() {
    // `time_pl_interval` adds the time field alone, wrapped at 24 hours.
    for (expression, answer) in [
        ("time '10:20:30.5' + interval '10000 years'", "10:20:30.5"),
        ("time '10:20:30.5' + interval '-2025 years 1 hour'", "11:20:30.5"),
        ("time '23:00:00' + interval '1 mon 2 days 36:00:00'", "11:00:00"),
        ("time '01:00:00' - interval '50:00:00'", "23:00:00"),
    ] {
        assert_eq!(
            run_translated_default(&format!("{SCHEMA}\nSELECT ({expression})::text;")),
            some(&[answer]),
            "{expression}"
        );
    }
}

/// An interval clock 12:00:00.000191 short of the 64-bit bound in
/// microseconds: `time_pl_interval` adds it to the time of day in 64 bits,
/// which wraps for a time of day past 12:00:00.000191 before the day is
/// taken off.
const NEAR_BOUND_CLOCK: &str = "2562047776:00:54.775616";

/// What [`NEAR_BOUND_CLOCK`] moves a time of day by when the sum stays in
/// 64 bits: the clock less its whole days.
const NEAR_BOUND_CLOCK_IN_A_DAY: &str = "16:00:54.775616";

#[test]
fn a_time_shift_past_the_64_bit_bound_wraps_as_postgresql_does() {
    let clock = NEAR_BOUND_CLOCK;
    let wrapped = [
        ("time", "12:00:00.001", "+", clock.to_string(), "19:59:05.225"),
        ("time", "23:59:59.999", "+", clock.to_string(), "07:59:05.223"),
        ("time", "12:00:00.001", "-", format!("-{clock}"), "19:59:05.225"),
        (
            "time",
            "12:00:00.001",
            "-",
            format!("-178956970 years -8 mons -2147483648 days -{clock}"),
            "19:59:05.225",
        ),
        ("timetz", "12:00:00.001-05:30", "+", clock.to_string(), "19:59:05.225-05:30"),
        ("timetz", "12:00:00.001+05:30", "-", format!("-{clock}"), "19:59:05.225+05:30"),
    ];
    for (kind, target, op, interval, answer) in &wrapped {
        let literal = format!("SELECT ({kind} '{target}' {op} interval '{interval}')::text;");
        assert_eq!(
            run_translated_default(&format!("{SCHEMA}\n{literal}")),
            some(&[answer]),
            "{literal}"
        );
        let column = format!(
            "CREATE TABLE w (tg {kind}, iv interval);\nINSERT INTO w VALUES ('{target}', '{interval}');\nSELECT (tg {op} iv)::text FROM w;"
        );
        assert_eq!(
            run_translated_default(&format!("{SCHEMA}\n{column}")),
            some(&[answer]),
            "{column}"
        );
    }
    // A time of day at or below the bound's distance stays in 64 bits and
    // moves as the clock's remainder in a day moves it.
    for (kind, target) in [("time", "12:00:00"), ("time", "00:00:00"), ("timetz", "12:00:00-05:30")]
    {
        let expected = run_translated_default(&format!(
            "{SCHEMA}\nSELECT ({kind} '{target}' + interval '{NEAR_BOUND_CLOCK_IN_A_DAY}')::text;"
        ));
        for (op, interval) in [("+", clock.to_string()), ("-", format!("-{clock}"))] {
            let literal = format!("SELECT ({kind} '{target}' {op} interval '{interval}')::text;");
            assert_eq!(
                run_translated_default(&format!("{SCHEMA}\n{literal}")),
                expected,
                "{literal}"
            );
            let column = format!(
                "CREATE TABLE w (tg {kind}, iv interval);\nINSERT INTO w VALUES ('{target}', '{interval}');\nSELECT (tg {op} iv)::text FROM w;"
            );
            assert_eq!(
                run_translated_default(&format!("{SCHEMA}\n{column}")),
                expected,
                "{column}"
            );
        }
    }
}

#[test]
fn a_time_shift_that_wraps_for_every_time_of_day_matches_postgresql() {
    // Twice this clock is -2^63 microseconds, so subtracting it leaves 64
    // bits for every time of day, and PostgreSQL answers the time of day
    // moved by 19:59:05.224192.
    for target in ["12:00:00.001", "00:00:00"] {
        let scaled = run_translated_default(&format!(
            "{SCHEMA}\nSELECT (time '{target}' - 2 * interval '-1281023894:00:27.387904')::text;"
        ));
        let reduced = run_translated_default(&format!(
            "{SCHEMA}\nSELECT (time '{target}' + interval '19:59:05.224192')::text;"
        ));
        assert_eq!(scaled, reduced, "{target}");
    }
}

#[test]
fn a_volatile_time_shifted_past_the_64_bit_bound_is_refused() {
    // The wrapping form reads the time of day for the bound and for each
    // answer, and `clock_timestamp()` answers anew on each read.
    let volatile = "CAST(clock_timestamp() AS time)";
    let wrapping = format!("SELECT ({volatile} + interval '{NEAR_BOUND_CLOCK}')::text FROM t;");
    refused(&wrapping, "64 bits");
    refused(&wrapping, "clock_timestamp");
    let within = format!("{SCHEMA}\nSELECT ({volatile} + interval '1 hour')::text FROM t;");
    assert!(
        Pg2Sqlite::default()
            .sql(&within)
            .expect("parse")
            .translate_to_sql(&Pg2SqliteOptions::default())
            .is_ok(),
        "a shift that stays in 64 bits reads the time of day once"
    );
}

#[test]
fn a_time_minus_an_interval_reads_only_its_clock() {
    // `time_mi_interval` subtracts the time field, so months and days with
    // no negation in 32 bits take no part.
    for (expression, answer) in [
        ("time '00:00:00' - interval '-2147483648 days'", "00:00:00"),
        ("time '10:00:00' - interval '-178956970 years -8 mons 01:00:00'", "09:00:00"),
        ("timetz '10:00:00-05:30' - interval '-2147483648 days 01:00:00'", "09:00:00-05:30"),
    ] {
        assert_eq!(
            run_translated_default(&format!("{SCHEMA}\nSELECT ({expression})::text;")),
            some(&[answer]),
            "{expression}"
        );
    }
}

#[test]
fn a_signed_factor_matches_postgresql() {
    let schema = "CREATE TABLE fz (id int, iv interval, k int);\nINSERT INTO fz VALUES (1, '1 day 02:00:00', 3), (2, '1 mon', -2);";
    for (expression, answers) in [
        ("iv * -k", ["-3 days -06:00:00", "2 mons"]),
        ("iv * +k", ["3 days 06:00:00", "-2 mons"]),
        ("iv / -(k)", ["-08:40:00", "15 days"]),
        ("interval '1 day 02:00:00' * k", ["3 days 06:00:00", "-2 days -04:00:00"]),
    ] {
        assert_eq!(
            run_translated_default(&format!(
                "{SCHEMA}\n{schema}\nSELECT ({expression})::text FROM fz ORDER BY id;"
            )),
            some(&answers),
            "{expression}"
        );
    }
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\nSELECT (interval '1 day' * -CAST(NULL AS int))::text;"
        )),
        vec![None]
    );
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (interval '1 mon' * -1.5)::text;")),
        some(&["-1 mons -15 days"])
    );
}

#[test]
fn a_digit_grouped_factor_matches_postgresql() {
    assert_eq!(
        run_translated_default(&format!("{SCHEMA}\nSELECT (interval '1 day' * 1_000)::text;")),
        some(&["1000 days"])
    );
}

#[test]
fn shifting_by_a_cast_interval_literal_matches_postgresql() {
    let schema = "CREATE TABLE sh3 (id int, d date, ts timestamp);\nINSERT INTO sh3 VALUES (1, '2026-01-31', '2026-01-31 10:00:00');";
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\n{schema}\nSELECT (d + CAST('1 mon' AS interval))::text FROM sh3;"
        )),
        some(&["2026-02-28 00:00:00"])
    );
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\n{schema}\nSELECT (ts - CAST('1 day 01:00:00' AS interval))::text FROM sh3;"
        )),
        some(&["2026-01-30 09:00:00"])
    );
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\n{schema}\nSELECT (d + CAST(NULL AS interval))::text FROM sh3;"
        )),
        vec![None]
    );
    refused(
        "SELECT (ts - CAST('-178956970 years -8 mons' AS interval))::text FROM t;",
        "interval out of range",
    );
}

#[test]
fn an_interval_minus_a_timestamp_is_refused_with_postgresqls_message() {
    refused(
        "SELECT (iv - ts)::text FROM t;",
        "operator does not exist: interval - timestamp with time zone",
    );
    // PostgreSQL casts the time to an interval and answers an interval.
    refused(
        "CREATE TABLE tm (iv interval, t time);\nSELECT (iv - t)::text FROM tm;",
        "PostgreSQL casts the time to an interval",
    );
}

#[test]
fn a_non_interval_side_is_refused_with_postgresqls_message() {
    // PostgreSQL settles NULLIF(NULL, NULL) to text, so the sum is refused the
    // way PostgreSQL refuses it, measured on PostgreSQL 18.6.
    refused(
        "SELECT (iv + NULLIF(NULL, NULL))::text FROM t;",
        "operator does not exist: interval + text",
    );
    refused(
        "SELECT (iv + CASE WHEN id > 1 THEN NULL END)::text FROM t;",
        "operator does not exist: interval + text",
    );
}

#[test]
fn a_shift_by_an_interval_column_in_an_index_is_refused() {
    refused("CREATE INDEX ON t (ts + iv);", "scalar subquery");
}

#[test]
fn an_aggregate_over_an_operand_with_an_exists_is_refused() {
    refused(
        "SELECT sum(CASE WHEN EXISTS (SELECT 1 FROM t) THEN iv END)::text FROM t;",
        "an interval sum has no SQLite form",
    );
}

#[test]
fn shifting_by_a_null_interval_answers_null() {
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\nSELECT (t.ts + n.iv)::text FROM n, t WHERE n.id = 2 AND t.id = 1;"
        )),
        vec![None]
    );
}

#[test]
fn a_sum_over_a_non_interval_keeps_its_ordinary_translation() {
    let got = run_translated_default(&format!("{SCHEMA}\nSELECT sum(id) FROM t;"));
    assert_eq!(got, vec![Some("6".to_string())]);
}

#[test]
fn a_sum_over_a_named_argument_is_refused_with_postgresqls_message() {
    refused("SELECT sum(a := iv) FROM t;", "function sum(a => interval) does not exist");
    // PostgreSQL orders the positional argument before the named one and
    // spells the refusal over the pair.
    refused(
        "SELECT sum(iv, a := iv2) FROM t;",
        "function sum(interval, a => interval) does not exist",
    );
}

#[test]
fn a_sum_over_a_named_wildcard_is_refused() {
    // PostgreSQL does not parse a wildcard beside a named argument; the
    // translation refuses the call either way.
    refused("SELECT sum(a := *, iv) FROM t;", "function sum(unknown, interval) does not exist");
}

#[test]
fn a_sum_without_arguments_is_not_an_interval_sum() {
    // Neither engine runs a sum without an argument; the interval dispatch
    // leaves the empty call to the ordinary sum.
    Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}\nSELECT sum() FROM t;"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("an empty sum is not an interval sum");
}

#[test]
fn a_sum_with_a_distinct_modifier_is_refused() {
    refused("SELECT sum(DISTINCT iv)::text FROM t;", "sum(DISTINCT) over an interval");
}

#[test]
fn a_sum_with_an_over_clause_is_refused() {
    refused("SELECT sum(iv) OVER () FROM t;", "OVER clause");
}

#[test]
fn a_sum_with_a_filter_clause_is_refused() {
    refused("SELECT sum(iv) FILTER (WHERE id = 1)::text FROM t;", "FILTER or WITHIN GROUP");
    refused("SELECT sum(iv) WITHIN GROUP (ORDER BY id) FROM t;", "FILTER or WITHIN GROUP");
    refused("SELECT sum(iv) RESPECT NULLS FROM t;", "FILTER or WITHIN GROUP");
}

#[test]
fn a_sum_over_a_pair_is_refused_with_postgresqls_message() {
    refused("SELECT sum(iv, iv2) FROM t;", "function sum(interval, interval) does not exist");
}

#[test]
fn a_sum_with_a_volatile_operand_is_refused() {
    refused(
        "SELECT sum(CASE WHEN random() > 0.5 THEN iv ELSE iv2 END)::text FROM t;",
        "an interval sum has no SQLite form",
    );
}

#[test]
fn interval_arithmetic_in_an_index_expression_is_refused() {
    refused("CREATE INDEX ON t (iv + iv2);", "scalar subquery");
    refused("CREATE INDEX ON t (iv - iv2);", "scalar subquery");
    refused("CREATE INDEX ON t (-iv);", "scalar subquery");
    refused("CREATE INDEX ON t (iv + interval '1 day');", "scalar subquery");
}

#[test]
fn interval_sum_in_a_generated_column_is_refused() {
    refused(
        "CREATE TABLE gen_s (g text GENERATED ALWAYS AS (sum(interval '1 day')) STORED)",
        "scalar subquery",
    );
}

#[test]
fn interval_arithmetic_in_a_generated_column_is_refused() {
    // PostgreSQL stores these generated columns and answers their text on
    // read; the replica has no render of the arithmetic where SQLite forbids
    // the scalar subquery the answer reads.
    refused(
        "CREATE TABLE gen_a (iv interval, iv2 interval, g interval GENERATED ALWAYS AS (iv + iv2) STORED);",
        "is not legal in that position",
    );
    refused(
        "CREATE TABLE gen_n (iv interval, g interval GENERATED ALWAYS AS (-iv) STORED);",
        "is not legal in that position",
    );
}

#[test]
fn an_infinite_literal_beside_an_interval_is_refused() {
    // PostgreSQL answers the arithmetic with the infinite interval it reads;
    // the replica has no representation for it until the infinity reads.
    for pg in [
        "SELECT (interval 'infinity' + interval '1 day')::text;",
        "SELECT (interval 'infinity' - interval '1 day')::text;",
        "SELECT (interval '-infinity' + interval '1 day')::text;",
        "SELECT (interval '-infinity' - interval '1 day')::text;",
        "SELECT (-interval 'infinity')::text;",
        "SELECT (-interval '-infinity')::text;",
        "SELECT ('infinity' + interval '1 day')::text;",
        "SELECT (interval '2147483647 mons 2147483647 days 2562047788:00:54.775807' + interval '1 day')::text;",
    ] {
        refused(pg, "is infinite in PostgreSQL");
    }
}

#[test]
fn a_literal_result_on_the_infinity_bound_is_refused() {
    // PostgreSQL raises interval out of range on a result that equals either
    // infinity's fields; the translation refuses it at the same step.
    refused(
        "SELECT (interval '2147483646 mons 2147483647 days 2562047788:00:54.775806' + interval '1 mon 00:00:00.000001')::text;",
        "interval out of range",
    );
    refused(
        "SELECT (interval '-2147483647 mons -2147483647 days -2562047788:00:54.775807' - interval '1 mon 1 day 00:00:00.000001')::text;",
        "interval out of range",
    );
}

#[test]
fn a_literal_result_past_the_negative_bound_stays_finite() {
    // One microsecond above the NOBEGIN fields the result stays finite in
    // PostgreSQL, and its text answers as is.
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (interval '-2147483647 mons -2147483647 days -2562047788:00:54.775807' \
         - interval '1 mon 00:00:00.000001')::text;"
    ));
    assert_eq!(
        got,
        vec![Some(
            "-178956970 years -8 mons -2147483647 days -2562047788:00:54.775808".to_string()
        )]
    );
}

#[test]
fn negating_past_the_microsecond_bound_answers_null() {
    // The inner subtraction answers the clock on the bound, whose negation
    // leaves the 64-bit range PostgreSQL's fields hold; PostgreSQL raises
    // interval out of range, and the replica answers NULL.
    let got = run_translated_default(&format!(
        "{SCHEMA}\nSELECT (-(interval '-2562047788:00:54.775807' - interval '00:00:00.000001'))::text;"
    ));
    assert_eq!(got, vec![None]);
}

#[test]
fn a_product_beside_an_interval_column_matches_postgresql() {
    assert_eq!(
        run_translated_default(&format!(
            "{SCHEMA}\nSELECT ((iv * 2) + iv2)::text FROM t ORDER BY id;"
        )),
        some(&["2 days 24:00:00", "2 mons 30 days", "22 days"])
    );
}

#[test]
fn a_number_beside_an_interval_from_the_left_is_refused_with_postgresqls_message() {
    refused("SELECT (5 + iv)::text FROM t;", "operator does not exist: integer + interval");
}

#[test]
fn a_bound_parameter_beside_an_interval_matches_postgresql() {
    let got = run_translated_bound_with(
        &format!("{SCHEMA}\nSELECT (iv + $1)::text FROM n ORDER BY id;"),
        &Pg2SqliteOptions::default(),
        &["2 days"],
    );
    assert_eq!(
        got,
        vec![
            Some("1 day".to_string()),
            Some("2 days".to_string()),
            Some("3 days".to_string()),
            None,
            Some("1 day 02:00:00".to_string()),
        ]
    );
    let got = run_translated_bound_with(
        &format!("{SCHEMA}\nSELECT (iv - $1)::text FROM n ORDER BY id;"),
        &Pg2SqliteOptions::default(),
        &["1 day"],
    );
    assert_eq!(
        got,
        vec![
            Some("-2 days".to_string()),
            Some("-1 days".to_string()),
            Some("00:00:00".to_string()),
            None,
            Some("-2 days +02:00:00".to_string()),
        ]
    );
}

#[test]
fn a_bound_parameter_that_postgresql_answers_infinity_answers_null() {
    // PostgreSQL answers the addition with infinity, which the replica has no
    // representation for, and answers NULL.
    for text in ["infinity", "-infinity"] {
        let got = run_translated_bound_with(
            &format!("{SCHEMA}\nSELECT (iv + $1)::text FROM n ORDER BY id;"),
            &Pg2SqliteOptions::default(),
            &[text],
        );
        assert_eq!(got, vec![None, None, None, None, None], "{text} beside n.iv");
    }
}
