//! Opening a database: Genesis, Config and open modes, What genesis writes, Reopening.

use golemdb_conformance::*;
use sha3::Digest;

fn h(algorithm: HashAlgorithm, bytes: &[u8]) -> [u8; 32] {
    match algorithm {
        HashAlgorithm::Keccak256 => sha3::Keccak256::digest(bytes).into(),
        HashAlgorithm::Blake3 => blake3::hash(bytes).into(),
    }
}

fn open(path: &std::path::Path, genesis: Genesis, mode: OpenMode) -> OpenResult<Database> {
    Database::open(path, &OpenConfig::new(genesis).with_mode(mode))
}

const SPEC_YAML: &str = "\
hash_function: keccak-256        # or blake3
cell_limits:
  max_cell_name_len: 32
  max_str_len: 64
  max_bytes_len: 128
record_keys: caller_assigned     # or:  record_keys: { generated: { seed: \"0x…\" } }
";

/// The spec's own genesis example parses.
#[test]
#[ignore = "spec-gap: record_keys (key modes) not implemented; Genesis rejects the field"]
fn the_spec_genesis_example_parses() {
    let genesis = Genesis::from_yaml(SPEC_YAML).unwrap();
    assert_eq!(genesis.hash_function, HashAlgorithm::Keccak256);
    assert_eq!(genesis.cell_limits, LIMITS);
}

/// "As YAML, every field is required; missing, duplicate and unknown fields are rejected"
#[test]
fn genesis_yaml_rejects_missing_duplicate_and_unknown_fields() {
    let limits = "cell_limits:\n  max_cell_name_len: 32\n  max_str_len: 64\n  max_bytes_len: 128\n";
    let base = format!("hash_function: blake3\n{limits}");
    let genesis = Genesis::from_yaml(&base).unwrap();
    assert_eq!(genesis.hash_function, HashAlgorithm::Blake3);
    assert_eq!(genesis.cell_limits, LIMITS);
    for (case, yaml) in [
        ("missing hash_function", limits.to_string()),
        ("missing cell_limits", "hash_function: blake3\n".to_string()),
        (
            "missing a limit",
            "hash_function: blake3\ncell_limits:\n  max_cell_name_len: 32\n  max_str_len: 64\n"
                .to_string(),
        ),
        ("duplicate field", format!("{base}hash_function: blake3\n")),
        (
            "duplicate limit",
            base.replace("max_str_len: 64\n", "max_str_len: 64\n  max_str_len: 65\n"),
        ),
        ("unknown field", format!("{base}extra: 1\n")),
        (
            "unknown limit",
            base.replace("max_str_len: 64\n", "max_str_len: 64\n  max_foo: 1\n"),
        ),
        ("unknown hash", base.replace("blake3", "sha256")),
        (
            "hash spelled differently",
            base.replace("blake3", "keccak256"),
        ),
    ] {
        let result = Genesis::from_yaml(&yaml);
        assert!(
            matches!(result, Err(OpenError::Yaml(_))),
            "{case}: {result:?}"
        );
    }
}

/// "`Genesis::load(path)` reads that one file."
#[test]
fn genesis_load_reads_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("genesis.yaml");
    std::fs::write(
        &path,
        "hash_function: keccak-256\ncell_limits: { max_cell_name_len: 32, max_str_len: 64, max_bytes_len: 128 }\n",
    )
    .unwrap();
    assert_eq!(Genesis::load(&path).unwrap(), genesis());
    assert!(matches!(
        Genesis::load(dir.path().join("missing.yaml")),
        Err(OpenError::Io(_))
    ));
}

/// "`max_cell_name_len` must be positive." "A failure is `OpenError::InvalidConfig`."
#[test]
fn zero_name_length_is_invalid_config() {
    let limits = CellLimits {
        max_cell_name_len: 0,
        ..LIMITS
    };
    let result = Database::open_memory(&Genesis::new(HashAlgorithm::Keccak256, limits, None));
    assert!(
        matches!(result, Err(OpenError::InvalidConfig(_))),
        "{:?}",
        result.map(drop)
    );
}

/// "Required key size: `max(name + 8, name + 2 + max(max_str_len, 32), 40)` bytes …
/// It must fit the store's maximum key." MDBX keys are at most a few KiB.
#[test]
fn limits_that_do_not_fit_an_mdbx_key_are_invalid_config() {
    let dir = tempfile::tempdir().unwrap();
    for limits in [
        CellLimits {
            max_str_len: 1 << 20,
            ..LIMITS
        },
        CellLimits {
            max_cell_name_len: 1 << 20,
            ..LIMITS
        },
    ] {
        let result = open(
            dir.path(),
            Genesis::new(HashAlgorithm::Keccak256, limits, None),
            OpenMode::CreateIfMissing,
        );
        assert!(
            matches!(result, Err(OpenError::InvalidConfig(_))),
            "{limits:?}: {:?}",
            result.map(drop)
        );
    }
    // Large `bytes` values are fine: they are values, not keys.
    let limits = CellLimits {
        max_bytes_len: 1 << 20,
        ..LIMITS
    };
    open(
        dir.path(),
        Genesis::new(HashAlgorithm::Keccak256, limits, None),
        OpenMode::CreateIfMissing,
    )
    .unwrap();
}

/// Open-mode table: `CreateIfMissing` | pristine: write genesis, commit 0 | initialized: validate and reopen.
#[test]
fn create_if_missing_creates_then_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let first = *open(dir.path(), genesis(), OpenMode::CreateIfMissing)
        .unwrap()
        .info();
    assert!(first.created);
    assert_eq!(first.commit_id, CommitId::GENESIS);
    let again = *open(dir.path(), genesis(), OpenMode::CreateIfMissing)
        .unwrap()
        .info();
    assert!(!again.created);
    assert_eq!(
        (
            again.commit_id,
            again.state_root,
            again.index_root,
            again.genesis_id
        ),
        (
            first.commit_id,
            first.state_root,
            first.index_root,
            first.genesis_id
        )
    );
}

/// `ExistingOnly` | pristine: `NotInitialized` | initialized: validate and reopen.
/// "With `ExistingOnly`, a missing directory is `NotInitialized` and nothing is created."
#[test]
fn existing_only_needs_an_initialized_store() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    assert!(matches!(
        open(&missing, genesis(), OpenMode::ExistingOnly),
        Err(OpenError::NotInitialized)
    ));
    assert!(!missing.exists());
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    assert!(matches!(
        open(&empty, genesis(), OpenMode::ExistingOnly),
        Err(OpenError::NotInitialized)
    ));
    open(&empty, genesis(), OpenMode::CreateNew).unwrap();
    assert!(
        !open(&empty, genesis(), OpenMode::ExistingOnly)
            .unwrap()
            .info()
            .created
    );
}

/// `CreateNew` | pristine: write genesis, commit 0 | initialized: validate, then `AlreadyInitialized`.
#[test]
fn create_new_refuses_an_initialized_store() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        open(dir.path(), genesis(), OpenMode::CreateNew)
            .unwrap()
            .info()
            .created
    );
    assert!(matches!(
        open(dir.path(), genesis(), OpenMode::CreateNew),
        Err(OpenError::AlreadyInitialized)
    ));
    // "validate, then": a mismatching genesis is reported as such.
    let other = Genesis::new(HashAlgorithm::Blake3, LIMITS, None);
    assert!(matches!(
        open(dir.path(), other, OpenMode::CreateNew),
        Err(OpenError::GenesisMismatch)
    ));
}

/// "Reopening … checks … `hash_fn` and `genesis_id` against the supplied genesis (`GenesisMismatch`)"
#[test]
fn reopening_with_another_genesis_is_genesis_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path(), genesis(), OpenMode::CreateNew).unwrap();
    for other in [
        Genesis::new(HashAlgorithm::Blake3, LIMITS, None),
        Genesis::new(
            HashAlgorithm::Keccak256,
            CellLimits {
                max_str_len: 65,
                ..LIMITS
            },
            None,
        ),
        Genesis::new(
            HashAlgorithm::Keccak256,
            CellLimits {
                max_cell_name_len: 31,
                ..LIMITS
            },
            None,
        ),
        Genesis::new(
            HashAlgorithm::Keccak256,
            CellLimits {
                max_bytes_len: 127,
                ..LIMITS
            },
            None,
        ),
    ] {
        for mode in [OpenMode::CreateIfMissing, OpenMode::ExistingOnly] {
            let result = open(dir.path(), other.clone(), mode);
            assert!(
                matches!(result, Err(OpenError::GenesisMismatch)),
                "{other:?}: {:?}",
                result.map(drop)
            );
        }
    }
    // The store is unharmed.
    open(dir.path(), genesis(), OpenMode::ExistingOnly).unwrap();
}

/// "Reopening never writes."
#[test]
fn reopening_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let config = OpenConfig::new(genesis());
    {
        let db = Database::open(dir.path(), &config).unwrap();
        commit_create(
            &db,
            RecordOp::create(key(1), B).attribute("a", CellValue::from_u32(1)),
        );
    }
    let infos: Vec<OpenInfo> = (0..3)
        .map(|_| *Database::open(dir.path(), &config).unwrap().info())
        .collect();
    assert!(infos.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(infos[0].commit_id, CommitId::new(1));
}

/// "`open_memory(&Genesis)` | A fresh in-memory database … every call starts empty."
#[test]
fn open_memory_starts_empty_every_time() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let fresh = Database::open_memory(&genesis()).unwrap();
    assert_eq!(fresh.head().unwrap(), CommitId::GENESIS);
    assert!(fresh.info().created);
    assert_err!(
        fresh.get(ReadTarget::Head, RecordOp::get(key(1), B)).result,
        ApiError::NotFound
    );
}

/// "`genesis() -> &Genesis`: the deployment settings validated at opening."
/// "`info()` … It does not follow the head; call `head()` for that."
#[test]
fn genesis_and_info_accessors() {
    let db = db();
    assert_eq!(db.genesis(), &genesis());
    let info = *db.info();
    commit_create(&db, RecordOp::create(key(1), B));
    assert_eq!(db.info(), &info);
    assert_eq!(db.head().unwrap(), CommitId::new(1));
}

/// "`head = (0, StateRoot, IndexRoot)` with `IndexRoot = H("")`, the empty root,
/// since no genesis cell is indexed."
#[test]
fn genesis_index_root_is_the_hash_of_nothing() {
    for algorithm in [HashAlgorithm::Keccak256, HashAlgorithm::Blake3] {
        let db = open_db(&Genesis::new(algorithm, LIMITS, None));
        assert_eq!(db.info().index_root, h(algorithm, b""), "{algorithm:?}");
    }
}

/// "Genesis cells, all of kind `field`"; "`#roots` and `#rootIndex` hold only their
/// identity cells. The two metering records hold only their identities and bindings."
#[test]
fn genesis_writes_only_the_listed_cells() {
    let db = db();
    for (name, extra) in [
        (
            "#params",
            &["#maxBytesLen", "#maxCellNameLen", "#maxStrLen"][..],
        ),
        ("#alloc", &["#nextRecordID"][..]),
        ("#roots", &[][..]),
        ("#rootIndex", &[][..]),
        ("@meteringModel", &[][..]),
        ("@modelWeight", &[][..]),
    ] {
        let record = read(&db, ReadTarget::Head, reserved_key(name));
        let mut names: Vec<_> = record
            .cells
            .keys()
            .map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned())
            .collect();
        names.retain(|n| n != "#key");
        assert_eq!(names, extra, "{name}");
        assert!(
            record.cells.values().all(|v| v.kind() == CellKind::Field),
            "{name}"
        );
    }
    // #recordKeys binds every reserved record.
    let bindings = read(&db, ReadTarget::Head, reserved_key("#recordKeys"));
    for (name, id) in SYSTEM_RECORDS {
        assert_eq!(
            bindings.cells.get(reserved_key(name).0.as_slice()),
            Some(&CellValue::from_u64(id)),
            "{name}"
        );
    }
}

/// The implemented reserved records and their IDs (spec "Reserved records").
const SYSTEM_RECORDS: [(&str, u64); 7] = [
    ("#params", 0),
    ("#alloc", 1),
    ("#roots", 2),
    ("#recordKeys", 3),
    ("#rootIndex", 4),
    ("@meteringModel", 32),
    ("@modelWeight", 33),
];

/// The genesis identity, computed from the spec's formula ("What genesis writes")
/// with our own hash implementations, caller-assigned keys.
fn spec_genesis_id(algorithm: HashAlgorithm, limits: CellLimits) -> [u8; 32] {
    let field = |type_id: u8, payload: &[u8]| [&[type_id][..], payload].concat();
    let cell =
        |id: u64, name: &[u8], value: Vec<u8>| ([&id.to_be_bytes()[..], name].concat(), value);
    let mut cells = Vec::new();
    for (name, id) in SYSTEM_RECORDS {
        let key = reserved_key(name).0;
        cells.push(cell(id, b"#key", field(11, &key)));
        cells.push(cell(3, &key, field(13, &id.to_be_bytes())));
    }
    cells.push(cell(
        0,
        b"#maxCellNameLen",
        field(12, &limits.max_cell_name_len.to_be_bytes()),
    ));
    cells.push(cell(
        0,
        b"#maxStrLen",
        field(12, &limits.max_str_len.to_be_bytes()),
    ));
    cells.push(cell(
        0,
        b"#maxBytesLen",
        field(12, &limits.max_bytes_len.to_be_bytes()),
    ));
    cells.push(cell(0, b"#keyMode", field(12, &0u32.to_be_bytes())));
    cells.push(cell(1, b"#nextRecordID", field(13, &64u64.to_be_bytes())));
    cells.sort();
    let hash_id: u16 = match algorithm {
        HashAlgorithm::Keccak256 => 1,
        HashAlgorithm::Blake3 => 2,
    };
    let mut bytes = b"golemdb/genesis/v1\0".to_vec();
    bytes.extend_from_slice(&1u32.to_be_bytes()); // format
    bytes.extend_from_slice(&hash_id.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes()); // roaring
    bytes.extend_from_slice(&(cells.len() as u32).to_be_bytes());
    for (key, value) in &cells {
        bytes.extend_from_slice(&(key.len() as u32).to_be_bytes());
        bytes.extend_from_slice(key);
        bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
        bytes.extend_from_slice(value);
    }
    h(algorithm, &bytes)
}

/// "genesis_id = H( "golemdb/genesis/v1" ‖ 0x00 ‖ format ‖ hash_id ‖ roaring ‖ cell
/// count ‖ for each genesis cell in key order: len(key) ‖ key ‖ len(value) ‖ value )"
#[test]
#[ignore = "spec-gap: #keyMode is not a genesis cell yet, so genesis_id differs from the spec's"]
fn genesis_id_follows_the_spec_formula() {
    for algorithm in [HashAlgorithm::Keccak256, HashAlgorithm::Blake3] {
        let db = open_db(&Genesis::new(algorithm, LIMITS, None));
        assert_eq!(
            db.info().genesis_id,
            spec_genesis_id(algorithm, LIMITS),
            "{algorithm:?}"
        );
    }
}

/// "YAML formatting and field order do not affect the identity"; every setting does.
#[test]
fn genesis_id_depends_on_settings_only() {
    let id = |yaml: &str| *open_db(&Genesis::from_yaml(yaml).unwrap()).info();
    let a = id(
        "hash_function: keccak-256\ncell_limits:\n  max_cell_name_len: 32\n  max_str_len: 64\n  max_bytes_len: 128\n",
    );
    let b = id(
        "cell_limits: {max_bytes_len: 128, max_str_len: 64, max_cell_name_len: 32}  # same\nhash_function: \"keccak-256\"\n",
    );
    assert_eq!(a.genesis_id, b.genesis_id);
    assert_eq!(a.state_root, b.state_root);
    let blake = id(
        "hash_function: blake3\ncell_limits: {max_cell_name_len: 32, max_str_len: 64, max_bytes_len: 128}\n",
    );
    let limits = id(
        "hash_function: keccak-256\ncell_limits: {max_cell_name_len: 32, max_str_len: 64, max_bytes_len: 129}\n",
    );
    assert_ne!(a.genesis_id, blake.genesis_id);
    assert_ne!(a.genesis_id, limits.genesis_id);
    assert_ne!(a.state_root, limits.state_root);
}

/// Identity and roots are a function of the genesis only: same on memory and MDBX stores.
#[test]
fn genesis_is_the_same_on_every_store() {
    let dir = tempfile::tempdir().unwrap();
    let mdbx = *open(dir.path(), genesis(), OpenMode::CreateNew)
        .unwrap()
        .info();
    let memory = *db().info();
    assert_eq!(mdbx, memory);
}
