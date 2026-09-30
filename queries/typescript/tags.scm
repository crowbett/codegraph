; codegraph tags for TypeScript / TSX (see go/tags.scm for capture conventions).

; ---- definitions ----------------------------------------------------------
(function_declaration
  name: (identifier) @name) @definition.function

(generator_function_declaration
  name: (identifier) @name) @definition.function

(function_signature
  name: (identifier) @name) @definition.function

(class_declaration
  name: (type_identifier) @name) @definition.class

(abstract_class_declaration
  name: (type_identifier) @name) @definition.class

(interface_declaration
  name: (type_identifier) @name) @definition.interface

(type_alias_declaration
  name: (type_identifier) @name) @definition.type

(enum_declaration
  name: (identifier) @name) @definition.enum

(internal_module
  name: (identifier) @name) @definition.module

(method_definition
  name: [(property_identifier) (private_property_identifier)] @name) @definition.method

(method_signature
  name: (property_identifier) @name) @definition.method

(abstract_method_signature
  name: (property_identifier) @name) @definition.method

(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression) (generator_function)]) @definition.function)

(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression) (generator_function)]) @definition.function)

(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name
      value: [(string) (number) (template_string) (object) (array)]) @definition.const))

(program
  (export_statement
    (lexical_declaration
      (variable_declarator
        name: (identifier) @name
        value: [(string) (number) (template_string) (object) (array)]) @definition.const)))

; module-level values (`export const computed: Factory = ... as any`): extract.rs keeps the
; exported ones whose value is not a function, class or literal.
(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name) @definition.value))

(program
  (variable_declaration
    (variable_declarator
      name: (identifier) @name) @definition.value))

(program
  (export_statement
    (lexical_declaration
      (variable_declarator
        name: (identifier) @name) @definition.value)))

(program
  (export_statement
    (variable_declaration
      (variable_declarator
        name: (identifier) @name) @definition.value)))

; ---- references -----------------------------------------------------------
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (member_expression
    object: (_) @qualifier
    property: [(property_identifier) (private_property_identifier)] @name)) @reference.call

(new_expression
  constructor: (identifier) @name) @reference.call

(new_expression
  constructor: (member_expression
    object: (_) @qualifier
    property: (property_identifier) @name)) @reference.call

(extends_clause
  value: (identifier) @name) @reference.inherit

(extends_clause
  value: (member_expression
    object: (_) @qualifier
    property: (property_identifier) @name)) @reference.inherit

(implements_clause
  (type_identifier) @name) @reference.impl

(implements_clause
  (generic_type
    name: (type_identifier) @name)) @reference.impl

(extends_type_clause
  type: (type_identifier) @name) @reference.inherit

(extends_type_clause
  type: (generic_type
    name: (type_identifier) @name)) @reference.inherit

(nested_type_identifier
  module: (_) @qualifier
  name: (type_identifier) @name) @reference.type

(type_identifier) @name @reference.type

; callbacks passed as values
(arguments
  (member_expression
    object: (_) @qualifier
    property: (property_identifier) @name) @reference.value)

(arguments
  (identifier) @name @reference.value)

; ---- imports / exports ----------------------------------------------------
(import_statement) @import

; names an import (or re-export) binds are references to the imported item
(import_specifier
  name: (identifier) @name) @reference.binding

(import_clause
  (identifier) @name) @reference.binding

(export_statement
  (export_clause
    (export_specifier
      name: (identifier) @name))
  source: (string)) @reference.binding

((variable_declarator
  name: (_)
  value: (call_expression
    function: (identifier) @_fn
    arguments: (arguments (string)))) @import
  (#eq? @_fn "require"))

(export_statement) @export

; re-exports: `export * from './x'`, `export { a as b } from './x'`
(export_statement
  source: (string)) @import

; names used as values elsewhere: `{ increment: inc }`, `{ setState }`, `x instanceof E`,
; `Flags.x`, `api.retry = retry`, `const f = g`, `return h`, `[a, b]`, `export { a as b }`.
; Only names defined or imported in the file survive extraction; locals never do.
(pair
  value: (identifier) @name) @reference.value

(shorthand_property_identifier) @name @reference.value

(binary_expression
  operator: "instanceof"
  right: (identifier) @name) @reference.value

(member_expression
  object: (identifier) @name) @reference.value

(assignment_expression
  right: (identifier) @name) @reference.value

(variable_declarator
  value: (identifier) @name) @reference.value

(return_statement
  (identifier) @name) @reference.value

(array
  (identifier) @name) @reference.value

(export_clause
  (export_specifier
    name: (identifier) @name)) @reference.value
(as_expression
  (identifier) @name) @reference.value

(satisfies_expression
  (identifier) @name) @reference.value

(type_query
  (identifier) @name) @reference.value

; more value positions: `cond ? A : B`, `(f)`, `...rest`, class field initializers
; (`handler = onClick`), bare decorators (`@observer`)
(ternary_expression
  consequence: (identifier) @name) @reference.value

(ternary_expression
  alternative: (identifier) @name) @reference.value

(parenthesized_expression
  (identifier) @name) @reference.value

(spread_element
  (identifier) @name) @reference.value

(decorator
  (identifier) @name) @reference.value

(public_field_definition
  value: (identifier) @name) @reference.value
