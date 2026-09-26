; codegraph tags for C (see go/tags.scm for capture conventions).
; Extra: @decl — a function declarator; the name is dug out in code so that
; pointer-returning functions (`char **f(void)`) are handled uniformly.

; ---- definitions ----------------------------------------------------------
(function_definition
  declarator: (_) @decl) @definition.function

(struct_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

(union_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

(enum_specifier
  name: (type_identifier) @name
  body: (_)) @definition.enum

(type_definition
  declarator: (type_identifier) @name) @definition.type

(preproc_def
  name: (identifier) @name) @definition.const

(preproc_function_def
  name: (identifier) @name) @definition.macro

; ---- references -----------------------------------------------------------
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (field_expression
    argument: (_) @qualifier
    field: (field_identifier) @name)) @reference.call

(type_identifier) @name @reference.type

; function pointers passed as values
(argument_list
  (identifier) @name @reference.value)

; ---- imports --------------------------------------------------------------
(preproc_include) @import
