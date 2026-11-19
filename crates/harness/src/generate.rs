//! Deterministic keys and records: same seed and stream, same operations.

use anyhow::{Result, bail};
use golemdb_api::{Budget, CellType, CellValue, RecordKey, RecordOp, Width, op};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rand_distr::Zipf;

use crate::config::{Distribution, Len, Shape, Type};

pub struct Generator {
    cells: Vec<Cell>,
    patch_all_cells: bool,
    rng: ChaCha8Rng,
    stream: u64,
    /// Value bytes generated since the last `take_bytes`.
    bytes: usize,
}

struct Cell {
    name: String,
    attribute: bool,
    ty: Type,
    cardinality: u64,
    zipf: Option<Zipf<f64>>,
    distribution: Distribution,
    len: Len,
    /// `unique` values generated so far.
    generated: u64,
}

impl Generator {
    /// Each writer uses its own `stream`, so writers never generate the same key.
    pub fn new(shape: &Shape, seed: u64, stream: u64) -> Result<Self> {
        let mut cells = Vec::new();
        for (specs, attribute) in [(&shape.attributes, true), (&shape.fields, false)] {
            for spec in specs {
                for i in 0..spec.repeat {
                    let name = if spec.repeat == 1 {
                        spec.name.clone()
                    } else {
                        format!("{}_{i}", spec.name)
                    };
                    let zipf = match spec.distribution {
                        Distribution::Zipf => Some(Zipf::new(spec.cardinality as f64, 1.0)?),
                        _ => None,
                    };
                    cells.push(Cell {
                        name,
                        attribute,
                        ty: spec.ty,
                        cardinality: spec.cardinality,
                        zipf,
                        distribution: spec.distribution,
                        len: spec.len.clone(),
                        generated: 0,
                    });
                }
            }
        }
        Ok(Self {
            cells,
            patch_all_cells: shape.patch_all_cells,
            rng: ChaCha8Rng::seed_from_u64(mix(seed) ^ stream),
            stream,
            bytes: 0,
        })
    }

    pub fn rng(&mut self) -> &mut ChaCha8Rng {
        &mut self.rng
    }

    /// Value bytes generated since the last call.
    pub fn take_bytes(&mut self) -> usize {
        std::mem::take(&mut self.bytes)
    }

    pub fn record(&mut self, key: RecordKey) -> RecordOp<op::Create> {
        let mut op = RecordOp::create(key, Budget::Unlimited);
        for i in 0..self.cells.len() {
            let value = self.value(i);
            let cell = &self.cells[i];
            op = if cell.attribute {
                op.attribute(&cell.name, value)
            } else {
                op.field(&cell.name, value)
            };
        }
        op
    }

    /// New values for every cell except `unique` ones (`patch_all_cells`), or for one random cell.
    pub fn patch(&mut self, key: RecordKey) -> RecordOp<op::Patch> {
        let cells: Vec<usize> = if self.patch_all_cells {
            (0..self.cells.len())
                .filter(|&i| self.cells[i].distribution != Distribution::Unique)
                .collect()
        } else {
            vec![self.rng.random_range(0..self.cells.len())]
        };
        let mut op = RecordOp::patch(key, Budget::Unlimited);
        for i in cells {
            let value = self.value(i);
            let cell = &self.cells[i];
            op = if cell.attribute {
                op.attribute(&cell.name, value)
            } else {
                op.field(&cell.name, value)
            };
        }
        op
    }

    fn value(&mut self, i: usize) -> CellValue {
        let cell = &self.cells[i];
        let len = match &cell.len {
            Len::Fixed(len) => *len,
            Len::Ranges(ranges) => {
                let mut roll = self
                    .rng
                    .random_range(0..ranges.iter().map(|r| r.weight).sum());
                let range = ranges
                    .iter()
                    .find(|r| {
                        let hit = roll < r.weight;
                        roll = roll.saturating_sub(r.weight);
                        hit
                    })
                    .expect("roll < total weight");
                self.rng.random_range(range.min..=range.max)
            }
        };
        let n = match (cell.distribution, &cell.zipf) {
            (Distribution::Uniform, _) => self.rng.random_range(0..cell.cardinality),
            (Distribution::Zipf, Some(zipf)) => self.rng.sample(zipf) as u64 - 1,
            _ => (self.stream << 40) | (cell.generated + 1),
        };
        let ty = cell.ty;
        if cell.distribution == Distribution::Unique {
            self.cells[i].generated += 1;
        }
        self.bytes += if ty == Type::U64 { 8 } else { len };
        match ty {
            Type::Bytes => {
                let mut bytes = vec![0u8; len];
                self.rng.fill(&mut bytes[..]);
                CellValue::from_bytes(&bytes)
            }
            ty => encode(ty, n, len),
        }
    }

    /// The value of rank `n` of attribute `name`, as records hold it: for
    /// `uniform` and `zipf`, the n-th of its values (zipf's 0 is the most
    /// common); for `unique`, the n-th value this stream generated (from 1).
    pub fn value_at(&self, name: &str, n: u64) -> Result<CellValue> {
        let cell = self.attribute(name)?;
        let Len::Fixed(len) = cell.len else {
            bail!("`{name}`: queries need a fixed `len`");
        };
        let n = if cell.distribution == Distribution::Unique {
            (self.stream << 40) | n
        } else {
            n
        };
        Ok(encode(cell.ty, n, len))
    }

    /// A rank of attribute `name` drawn as records draw their values: a hot
    /// zipf value comes up often, a tail one rarely. For `unique`, any value
    /// generated so far (its record may since have been deleted).
    pub fn random_rank(&self, name: &str, rng: &mut impl Rng) -> Result<u64> {
        let cell = self.attribute(name)?;
        Ok(match (cell.distribution, &cell.zipf) {
            (Distribution::Uniform, _) => rng.random_range(0..cell.cardinality),
            (Distribution::Zipf, Some(zipf)) => rng.sample(zipf) as u64 - 1,
            _ => rng.random_range(1..=cell.generated.max(1)),
        })
    }

    /// The cell type of attribute `name`.
    pub fn type_of(&self, name: &str) -> Result<CellType> {
        Ok(match self.attribute(name)?.ty {
            Type::U64 => CellType::Uint(Width::W8),
            _ => CellType::Str,
        })
    }

    fn attribute(&self, name: &str) -> Result<&Cell> {
        match self.cells.iter().find(|c| c.attribute && c.name == name) {
            Some(cell) => Ok(cell),
            None => bail!("`{name}` is not an attribute of the shape"),
        }
    }
}

/// Value `n` of a `u64` or `str` cell of `len` bytes.
fn encode(ty: Type, n: u64, len: usize) -> CellValue {
    match ty {
        Type::U64 => CellValue::from_u64(n),
        _ => CellValue::from_str(&format!("{n:0>len$x}")),
    }
}

/// Pseudo-random 32-byte key, unique per (seed, stream, n).
pub fn key(seed: u64, stream: u64, n: u64) -> RecordKey {
    let mut state = mix(seed ^ mix(stream ^ mix(n)));
    let mut key = [0u8; 32];
    for chunk in key.chunks_exact_mut(8) {
        state = mix(state);
        chunk.copy_from_slice(&state.to_be_bytes());
    }
    RecordKey(key)
}

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
