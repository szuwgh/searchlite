//域接口定义
pub(crate) struct Field {
    id: u32,
    value: Value,
}

pub(crate) struct Vector {}

impl Field {
    fn new() {}
}

pub(crate) enum Value {
    Str(String),
    Vector(Vector),
}