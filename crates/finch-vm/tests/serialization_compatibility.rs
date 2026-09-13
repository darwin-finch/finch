use finch_vm::{
    CapabilityGrant, Module, TypedRuntimeCheckpoint, TypedValue, VerifiedModule, VmSideEffect,
};
use serde::{de::DeserializeOwned, Serialize};

fn assert_golden_round_trip<T>(name: &str, fixture: &[u8])
where
    T: DeserializeOwned + Serialize,
{
    let value: T = serde_json::from_slice(fixture)
        .unwrap_or_else(|error| panic!("pre-extraction {name} fixture must decode: {error}"));
    let mut encoded = serde_json::to_vec(&value)
        .unwrap_or_else(|error| panic!("decoded {name} fixture must encode: {error}"));
    encoded.push(b'\n');
    assert_eq!(
        encoded, fixture,
        "pre-extraction {name} bytes must remain identical after decode and re-encode"
    );
}

#[test]
fn test_pre_extraction_serialization_goldens_remain_byte_identical() {
    assert_eq!(
        finch_vm::VM_TYPE_SYSTEM_VERSION,
        5,
        "typed VM wire/checkpoint version must remain frozen at 5"
    );
    assert_golden_round_trip::<Module>("Module", include_bytes!("fixtures/module.json"));
    assert_golden_round_trip::<VerifiedModule>(
        "VerifiedModule",
        include_bytes!("fixtures/verified_module.json"),
    );
    assert_golden_round_trip::<TypedValue>(
        "TypedValue",
        include_bytes!("fixtures/typed_value.json"),
    );
    assert_golden_round_trip::<CapabilityGrant>(
        "CapabilityGrant",
        include_bytes!("fixtures/capability_grant.json"),
    );
    assert_golden_round_trip::<VmSideEffect>(
        "VmSideEffect/UiOperation",
        include_bytes!("fixtures/vm_side_effect.json"),
    );
    assert_golden_round_trip::<TypedRuntimeCheckpoint>(
        "TypedRuntimeCheckpoint",
        include_bytes!("fixtures/typed_runtime_checkpoint.json"),
    );
}
