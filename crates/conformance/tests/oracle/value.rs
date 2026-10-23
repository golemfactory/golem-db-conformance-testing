//! Cell values for the SQLite oracle, written from the spec's type table, not
//! from golem-db's codec.
//!
//! Each value has an **order key**: bytes whose memcmp order is the type's
//! order ("numeric for numbers and dates, false before true, byte order for
//! strings (UTF-8) and fixed bytes", IEEE total order for floats with `-0.0`
//! normalized to `+0.0`). SQLite compares BLOBs with memcmp, so equality and
//! ranges in SQL need nothing else. The key is also injective per type and as
//! long as the spec's payload, so it doubles as the stored value and as
//! `|value|` in the size formulas.

use golemdb_api::{CellType, CellValue, FloatWidth, Width};

/// The cell types the oracle generates (the 256-bit ones are covered by the spec tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ty {
    Bool,
    Str,
    Bytes,
    Bytes4,
    U32,
    U64,
    U128,
    I32,
    I64,
    I128,
    Dec64,
    F32,
    F64,
    Date32,
    Timestamp64,
}

impl Ty {
    pub const ALL: [Ty; 15] = [
        Ty::Bool,
        Ty::Str,
        Ty::Bytes,
        Ty::Bytes4,
        Ty::U32,
        Ty::U64,
        Ty::U128,
        Ty::I32,
        Ty::I64,
        Ty::I128,
        Ty::Dec64,
        Ty::F32,
        Ty::F64,
        Ty::Date32,
        Ty::Timestamp64,
    ];

    /// The type ID from the spec's type table.
    pub fn id(self) -> u8 {
        match self {
            Ty::Bool => 1,
            Ty::Str => 2,
            Ty::Bytes => 3,
            Ty::Bytes4 => 8,
            Ty::U32 => 12,
            Ty::U64 => 13,
            Ty::U128 => 14,
            Ty::I32 => 16,
            Ty::I64 => 17,
            Ty::I128 => 18,
            Ty::Dec64 => 21,
            Ty::F32 => 24,
            Ty::F64 => 25,
            Ty::Date32 => 28,
            Ty::Timestamp64 => 29,
        }
    }

    pub fn cell_type(self) -> CellType {
        match self {
            Ty::Bool => CellType::Bool,
            Ty::Str => CellType::Str,
            Ty::Bytes => CellType::Bytes,
            Ty::Bytes4 => CellType::FixedBytes(Width::W4),
            Ty::U32 => CellType::Uint(Width::W4),
            Ty::U64 => CellType::Uint(Width::W8),
            Ty::U128 => CellType::Uint(Width::W16),
            Ty::I32 => CellType::Int(Width::W4),
            Ty::I64 => CellType::Int(Width::W8),
            Ty::I128 => CellType::Int(Width::W16),
            Ty::Dec64 => CellType::Decimal(Width::W8),
            Ty::F32 => CellType::Float(FloatWidth::F32),
            Ty::F64 => CellType::Float(FloatWidth::F64),
            Ty::Date32 => CellType::Date32,
            Ty::Timestamp64 => CellType::Timestamp64,
        }
    }

    pub fn from_id(id: u8) -> Ty {
        Ty::ALL
            .into_iter()
            .find(|t| t.id() == id)
            .expect("known type id")
    }
}

/// A typed value.
#[derive(Debug, Clone, PartialEq)]
pub enum Tv {
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
    Bytes4([u8; 4]),
    U32(u32),
    U64(u64),
    U128(u128),
    I32(i32),
    I64(i64),
    I128(i128),
    Dec64(i64),
    F32(f32),
    F64(f64),
    Date32(i32),
    Timestamp64(i64),
}

impl Tv {
    pub fn ty(&self) -> Ty {
        match self {
            Tv::Bool(_) => Ty::Bool,
            Tv::Str(_) => Ty::Str,
            Tv::Bytes(_) => Ty::Bytes,
            Tv::Bytes4(_) => Ty::Bytes4,
            Tv::U32(_) => Ty::U32,
            Tv::U64(_) => Ty::U64,
            Tv::U128(_) => Ty::U128,
            Tv::I32(_) => Ty::I32,
            Tv::I64(_) => Ty::I64,
            Tv::I128(_) => Ty::I128,
            Tv::Dec64(_) => Ty::Dec64,
            Tv::F32(_) => Ty::F32,
            Tv::F64(_) => Ty::F64,
            Tv::Date32(_) => Ty::Date32,
            Tv::Timestamp64(_) => Ty::Timestamp64,
        }
    }

    /// The order key; see the module docs.
    pub fn key(&self) -> Vec<u8> {
        fn signed(v: i128, bytes: usize) -> Vec<u8> {
            let flipped = (v as u128) ^ (1u128 << (bytes * 8 - 1));
            flipped.to_be_bytes()[16 - bytes..].to_vec()
        }
        match self {
            Tv::Bool(b) => vec![*b as u8],
            Tv::Str(s) => s.as_bytes().to_vec(),
            Tv::Bytes(b) => b.clone(),
            Tv::Bytes4(b) => b.to_vec(),
            Tv::U32(v) => v.to_be_bytes().to_vec(),
            Tv::U64(v) => v.to_be_bytes().to_vec(),
            Tv::U128(v) => v.to_be_bytes().to_vec(),
            Tv::I32(v) | Tv::Date32(v) => signed(*v as i128, 4),
            Tv::I64(v) | Tv::Dec64(v) | Tv::Timestamp64(v) => signed(*v as i128, 8),
            Tv::I128(v) => signed(*v, 16),
            Tv::F32(f) => {
                let bits = (if *f == 0.0 { 0.0 } else { *f }).to_bits();
                let key = if bits >> 31 == 1 {
                    !bits
                } else {
                    bits | 1 << 31
                };
                key.to_be_bytes().to_vec()
            }
            Tv::F64(f) => {
                let bits = (if *f == 0.0 { 0.0 } else { *f }).to_bits();
                let key = if bits >> 63 == 1 {
                    !bits
                } else {
                    bits | 1 << 63
                };
                key.to_be_bytes().to_vec()
            }
        }
    }

    /// The inverse of [`Tv::key`].
    pub fn from_key(ty: Ty, key: &[u8]) -> Tv {
        fn unsigned(key: &[u8]) -> u128 {
            key.iter().fold(0, |acc, b| acc << 8 | *b as u128)
        }
        fn signed(key: &[u8]) -> i128 {
            let bits = key.len() * 8;
            let v = unsigned(key) ^ (1u128 << (bits - 1));
            // Sign-extend from `bits`.
            ((v << (128 - bits)) as i128) >> (128 - bits)
        }
        match ty {
            Ty::Bool => Tv::Bool(key[0] == 1),
            Ty::Str => Tv::Str(String::from_utf8(key.to_vec()).expect("utf-8")),
            Ty::Bytes => Tv::Bytes(key.to_vec()),
            Ty::Bytes4 => Tv::Bytes4(key.try_into().unwrap()),
            Ty::U32 => Tv::U32(unsigned(key) as u32),
            Ty::U64 => Tv::U64(unsigned(key) as u64),
            Ty::U128 => Tv::U128(unsigned(key)),
            Ty::I32 => Tv::I32(signed(key) as i32),
            Ty::I64 => Tv::I64(signed(key) as i64),
            Ty::I128 => Tv::I128(signed(key)),
            Ty::Dec64 => Tv::Dec64(signed(key) as i64),
            Ty::Date32 => Tv::Date32(signed(key) as i32),
            Ty::Timestamp64 => Tv::Timestamp64(signed(key) as i64),
            Ty::F32 => {
                let key = u32::from_be_bytes(key.try_into().unwrap());
                let bits = if key >> 31 == 1 {
                    key & !(1 << 31)
                } else {
                    !key
                };
                Tv::F32(f32::from_bits(bits))
            }
            Ty::F64 => {
                let key = u64::from_be_bytes(key.try_into().unwrap());
                let bits = if key >> 63 == 1 {
                    key & !(1 << 63)
                } else {
                    !key
                };
                Tv::F64(f64::from_bits(bits))
            }
        }
    }

    /// The value as golem-db's API builds it (a field; the builder sets the kind).
    pub fn cell(&self) -> CellValue {
        match self {
            Tv::Bool(b) => CellValue::from_bool(*b),
            Tv::Str(s) => CellValue::from_str(s),
            Tv::Bytes(b) => CellValue::from_bytes(b),
            Tv::Bytes4(b) => CellValue::from_bytes4(*b),
            Tv::U32(v) => CellValue::from_u32(*v),
            Tv::U64(v) => CellValue::from_u64(*v),
            Tv::U128(v) => CellValue::from_u128(*v),
            Tv::I32(v) => CellValue::from_i32(*v),
            Tv::I64(v) => CellValue::from_i64(*v),
            Tv::I128(v) => CellValue::from_i128(*v),
            Tv::Dec64(v) => CellValue::from_dec64_unscaled(*v),
            Tv::F32(f) => CellValue::from_f32(*f).expect("not NaN"),
            Tv::F64(f) => CellValue::from_f64(*f).expect("not NaN"),
            Tv::Date32(v) => CellValue::from_date32(*v),
            Tv::Timestamp64(v) => CellValue::from_timestamp64(*v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order key sorts as the type's order, and decodes back.
    #[test]
    fn order_keys_sort_as_the_values() {
        fn check(values: Vec<Tv>, cmp: impl Fn(&Tv, &Tv) -> std::cmp::Ordering) {
            for a in &values {
                let back = Tv::from_key(a.ty(), &a.key());
                assert_eq!(back.key(), a.key(), "{a:?}");
                for b in &values {
                    assert_eq!(a.key().cmp(&b.key()), cmp(a, b), "{a:?} vs {b:?}");
                }
            }
        }
        let ints = [
            i64::MIN,
            i32::MIN as i64 - 1,
            -256,
            -1,
            0,
            1,
            255,
            256,
            i64::MAX,
        ];
        check(ints.iter().map(|v| Tv::I64(*v)).collect(), |a, b| {
            match (a, b) {
                (Tv::I64(a), Tv::I64(b)) => a.cmp(b),
                _ => unreachable!(),
            }
        });
        check(
            [i128::MIN, -1, 0, 1, i128::MAX]
                .iter()
                .map(|v| Tv::I128(*v))
                .collect(),
            |a, b| match (a, b) {
                (Tv::I128(a), Tv::I128(b)) => a.cmp(b),
                _ => unreachable!(),
            },
        );
        check(
            [i32::MIN, -1, 0, 1, i32::MAX]
                .iter()
                .map(|v| Tv::Date32(*v))
                .collect(),
            |a, b| match (a, b) {
                (Tv::Date32(a), Tv::Date32(b)) => a.cmp(b),
                _ => unreachable!(),
            },
        );
        check(
            [0, 1, u128::MAX / 2, u128::MAX]
                .iter()
                .map(|v| Tv::U128(*v))
                .collect(),
            |a, b| match (a, b) {
                (Tv::U128(a), Tv::U128(b)) => a.cmp(b),
                _ => unreachable!(),
            },
        );
        let floats = [
            f64::NEG_INFINITY,
            f64::MIN,
            -1.5,
            -f64::MIN_POSITIVE,
            0.0,
            f64::MIN_POSITIVE,
            1.5,
            f64::MAX,
            f64::INFINITY,
        ];
        check(
            floats.iter().map(|f| Tv::F64(*f)).collect(),
            |a, b| match (a, b) {
                (Tv::F64(a), Tv::F64(b)) => a.total_cmp(b),
                _ => unreachable!(),
            },
        );
        let floats = [f32::NEG_INFINITY, -1.5, 0.0, 1.5, f32::INFINITY];
        check(
            floats.iter().map(|f| Tv::F32(*f)).collect(),
            |a, b| match (a, b) {
                (Tv::F32(a), Tv::F32(b)) => a.total_cmp(b),
                _ => unreachable!(),
            },
        );
        // -0.0 is +0.0.
        assert_eq!(Tv::F64(-0.0).key(), Tv::F64(0.0).key());
        assert_eq!(Tv::F32(-0.0).key(), Tv::F32(0.0).key());
    }
}
