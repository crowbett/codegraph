; codegraph tags for C++ (see go/tags.scm and c/tags.scm for capture conventions).

; ---- definitions ----------------------------------------------------------
(function_definition
  declarator: (_) @decl) @definition.function

(field_declaration
  declarator: (function_declarator) @decl) @definition.method

(class_specifier
  name: (type_identifier) @name
  body: (_)) @definition.class

(struct_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

(union_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

(enum_specifier
  name: (type_identifier) @name
  body: (_)) @definition.enum

(namespace_definition
  name: (namespace_identifier) @name) @definition.module

(type_definition
  declarator: (type_identifier) @name) @definition.type

(alias_declaration
  name: (type_identifier) @name) @definition.type

(concept_definition
  name: (identifier) @name) @definition.type

(preproc_def
  name: (identifier) @name) @definition.const

(preproc_function_def
  name: (identifier) @name) @definition.macro

; ---- references -----------------------------------------------------------
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (qualified_identifier
    scope: (_) @qualifier
    name: (identifier) @name)) @reference.call

(call_expression
  function: (field_expression
    argument: (_) @qualifier
    field: (field_identifier) @name)) @reference.call

(call_expression
  function: (template_function
    name: (identifier) @name)) @reference.call

(call_expression
  function: (qualified_identifier
    scope: (_) @qualifier
    name: (template_function
      name: (identifier) @name))) @reference.call

(new_expression
  type: (type_identifier) @name) @reference.call

(base_class_clause
  (type_identifier) @name) @reference.inherit

(base_class_clause
  (qualified_identifier
    scope: (_) @qualifier
    name: (type_identifier) @name)) @reference.inherit

(qualified_identifier
  scope: (namespace_identifier) @qualifier
  name: (type_identifier) @name) @reference.type

(type_identifier) @name @reference.type

; function pointers passed as values
(argument_list
  (identifier) @name @reference.value)

; ---- imports --------------------------------------------------------------
(preproc_include) @import
