use crate::fxhash::HashMap;
use crate::native::ir::LlType;
use spirv::{StorageClass, Word};

pub(crate) struct TypeInterner {
    pub(crate) types: HashMap<LlType, Word>,
    pub(crate) signed_int_types: HashMap<LlType, Word>,
    pub(crate) ptr_types: HashMap<(StorageClass, LlType), Word>,
    pub(crate) function_types: HashMap<Vec<Word>, Word>,
    pub(crate) uint_constants: HashMap<u32, Word>,
    pub(crate) int_constants: HashMap<(u32, u64), Word>,
    pub(crate) bool_constants: HashMap<bool, Word>,
    pub(crate) float32_constants: HashMap<u32, Word>,
    pub(crate) float16_constants: HashMap<u16, Word>,
    pub(crate) null_constants: HashMap<LlType, Word>,
    pub(crate) composite_constants: HashMap<(LlType, Vec<Word>), Word>,
    pub(crate) undefs: HashMap<LlType, Word>,
}

impl TypeInterner {
    pub(crate) fn new() -> Self {
        Self {
            types: HashMap::default(),
            signed_int_types: HashMap::default(),
            ptr_types: HashMap::default(),
            function_types: HashMap::default(),
            uint_constants: HashMap::default(),
            int_constants: HashMap::default(),
            bool_constants: HashMap::default(),
            float32_constants: HashMap::default(),
            float16_constants: HashMap::default(),
            null_constants: HashMap::default(),
            composite_constants: HashMap::default(),
            undefs: HashMap::default(),
        }
    }
}
