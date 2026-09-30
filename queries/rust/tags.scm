; codegraph tags for Rust (see go/tags.scm for capture conventions).
; Extra: @impl_for — the implementing type of `impl Trait for Type`.

; ---- definitions ----------------------------------------------------------
(function_item
  name: (identifier) @name) @definition.function

(function_signature_item
  name: (identifier) @name) @definition.function

(struct_item
  name: (type_identifier) @name) @definition.struct

(union_item
  name: (type_identifier) @name) @definition.struct

(enum_item
  name: (type_identifier) @name) @definition.enum

(type_item
  name: (type_identifier) @name) @definition.type

(trait_item
  name: (type_identifier) @name) @definition.trait

(mod_item
  name: (identifier) @name) @definition.module

(macro_definition
  name: (identifier) @name) @definition.macro

(const_item
  name: (identifier) @name) @definition.const

(static_item
  name: (identifier) @name) @definition.const

; ---- references -----------------------------------------------------------
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (field_expression
    value: (_) @qualifier
    field: (field_identifier) @name)) @reference.call

(call_expression
  function: (scoped_identifier
    path: (_) @qualifier
    name: (identifier) @name)) @reference.call

(call_expression
  function: (generic_function
    function: (identifier) @name)) @reference.call

(call_expression
  function: (generic_function
    function: (scoped_identifier
      path: (_) @qualifier
      name: (identifier) @name))) @reference.call

(call_expression
  function: (generic_function
    function: (field_expression
      value: (_) @qualifier
      field: (field_identifier) @name))) @reference.call

(macro_invocation
  macro: (identifier) @name) @reference.call

(struct_expression
  name: (type_identifier) @name) @reference.type

(scoped_type_identifier
  path: (_) @qualifier
  name: (type_identifier) @name) @reference.type

(type_identifier) @name @reference.type

; types in path-qualifier position: `Store::open(..)`, `ErrorCode::Eof`, `N::Float(_)`,
; `crate::error::Error::Locked(..)`. Paths are plain identifiers there; the capital
; letter tells a type from a module (`fs::read`).
(scoped_identifier
  path: (identifier) @name
  (#match? @name "^[A-Z]")) @reference.type

(scoped_identifier
  path: (scoped_identifier
    path: (_) @qualifier
    name: (identifier) @name)
  (#match? @name "^[A-Z]")) @reference.type

(token_tree
  (identifier) @name
  .
  "::"
  .
  (identifier)
  (#match? @name "^[A-Z]")) @reference.type

(impl_item
  trait: (type_identifier) @name
  type: (_) @impl_for) @reference.impl

(impl_item
  trait: (scoped_type_identifier
    name: (type_identifier) @name)
  type: (_) @impl_for) @reference.impl

(impl_item
  trait: (generic_type
    type: (type_identifier) @name)
  type: (_) @impl_for) @reference.impl

; supertraits (`trait Float: Number + ops::Add`): methods of `Number` are callable on a
; `T: Float`
(trait_item
  bounds: (trait_bounds
    (type_identifier) @name)) @reference.inherit

(trait_item
  bounds: (trait_bounds
    (generic_type
      type: (type_identifier) @name))) @reference.inherit

(trait_item
  bounds: (trait_bounds
    (scoped_type_identifier
      name: (type_identifier) @name))) @reference.inherit

; calls inside macro arguments: token trees are not parsed as expressions, so
; `assert_eq!(f(x), y)`, `vec![t(1)]`, `format!("{}", id.short())` are matched on tokens.
(token_tree
  (identifier) @name
  .
  (token_tree . "(")) @reference.call

(token_tree
  [(identifier) (self) (token_tree)] @qualifier
  .
  "."
  .
  (identifier) @name
  .
  (token_tree . "(")) @reference.call

(token_tree
  (identifier) @qualifier
  .
  "::"
  .
  (identifier) @name
  .
  (token_tree . "(")) @reference.call

; `Type::default()` in macro arguments (`default` is a keyword token there)
(token_tree
  (identifier) @qualifier
  .
  "::"
  .
  "default" @name
  .
  (token_tree . "(")) @reference.call

; turbofish calls in macro arguments: `from_str::<Value>(..)`, `x.into_float::<f64>()`,
; `from_str::<BTreeMap<u128, ()>>(j)`, `serde_json::from_str::<T>(..)`. The query only
; anchors on `name :: <`; extract.rs checks the `<..>` closes and a `(..)` follows.
(token_tree
  (identifier) @name
  .
  "::"
  .
  "<") @reference.call

(token_tree
  [(identifier) (self) (token_tree)] @qualifier
  .
  "."
  .
  (identifier) @name
  .
  "::"
  .
  "<") @reference.call

(token_tree
  (identifier) @qualifier
  .
  "::"
  .
  (identifier) @name
  .
  "::"
  .
  "<") @reference.call

; functions passed as values (`.map(Self::parse)`, `spawn(worker)`)
(arguments
  (scoped_identifier
    path: (_) @qualifier
    name: (identifier) @name) @reference.value)

(arguments
  (identifier) @name @reference.value)

; ---- imports --------------------------------------------------------------
(use_declaration) @import

; names a `use` binds are references to the imported item
(use_declaration
  argument: (identifier) @name) @reference.binding

(use_declaration
  argument: (scoped_identifier
    name: (identifier) @name)) @reference.binding

(use_list
  (identifier) @name) @reference.binding

(use_list
  (scoped_identifier
    name: (identifier) @name)) @reference.binding

(use_as_clause
  path: (identifier) @name) @reference.binding

(use_as_clause
  path: (scoped_identifier
    name: (identifier) @name)) @reference.binding
