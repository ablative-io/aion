//// gleeunit entry point. Discovers and runs every `*_test` function in the
//// test/ directory, including the generated `aion_order_saga_wire_compat_test`
//// goldens that pin each activity value type's wire shape.

import gleeunit

pub fn main() {
  gleeunit.main()
}
