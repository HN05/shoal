from pathlib import Path
import tempfile
import unittest

import check_stored_json as check


class CheckStoredJsonTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def write(self, path, text):
        path = self.root / path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def found(self, directories=("crates/core/",)):
        return [(problem.path, problem.line, problem.function)
                for problem in check.problems(self.root, directories)]

    def test_a_path_read_in_a_crate_s_source_is_refused_with_its_file_line_and_function(self):
        self.write("crates/core/src/lib.rs",
                   'fn f() {\n    db.query_row(\n        "SELECT id FROM t\n'
                   "            WHERE json_extract(record,'$.x')=?1\",\n    );\n}\n")
        self.assertEqual(self.found(), [("crates/core/src/lib.rs", 4, "json_extract")])

    def test_path_functions_operators_and_builders_are_refused_in_sql_files_and_tests(self):
        self.write("crates/core/tests/queries.sql",
                   "SELECT record ->> '$.kind', json_set(record,'$.a',1) FROM t;\n"
                   "SELECT json_group_array(id) FROM t;\n")
        self.write("crates/core/src/store/tests.rs",
                   "const ROW: &str = r#\"INSERT INTO t VALUES (json_object('a',1))\"#;\n")
        self.assertEqual(self.found(), [
            ("crates/core/src/store/tests.rs", 1, "json_object"),
            ("crates/core/tests/queries.sql", 1, "->>"),
            ("crates/core/tests/queries.sql", 1, "json_set"),
            ("crates/core/tests/queries.sql", 2, "json_group_array"),
        ])

    def test_a_function_is_refused_whatever_its_case(self):
        self.write("crates/core/src/lib.rs",
                   "const A: &str = \"SELECT JSON_EXTRACT(record,'$.kind') FROM t\";\n"
                   "const B: &str = \"SELECT Json_Each(t.files), JSON_EACH(?1) FROM t\";\n"
                   "const C: &str = \"SELECT JOURNAL_APPEND('x', id, JSON_OBJECT('a', 1))\";\n")
        self.assertEqual(self.found(), [("crates/core/src/lib.rs", 1, "JSON_EXTRACT"),
                                        ("crates/core/src/lib.rs", 2, "Json_Each")])

    def test_strings_quoted_names_and_comments_hold_no_sql(self):
        self.write("crates/core/tests/queries.sql",
                   "SELECT record FROM t WHERE name='--' AND json_extract(record,'$.kind')=1;\n"
                   "SELECT 'json_extract(record)', \"json_set(\", [json_array(] FROM t;\n"
                   "SELECT 'it''s' /* json_object( */, json_type(record) FROM t;\n")
        self.write("crates/core/src/lib.rs",
                   'const Q: &str = "SELECT \\"--\\", json_patch(a, b) FROM t";\n')
        self.assertEqual(self.found(), [("crates/core/src/lib.rs", 1, "json_patch"),
                                        ("crates/core/tests/queries.sql", 1, "json_extract"),
                                        ("crates/core/tests/queries.sql", 3, "json_type")])

    def test_a_rust_escape_reads_as_the_character_it_stands_for(self):
        self.write("crates/core/src/lib.rs",
                   'const A: &str = "SELECT 1 -- note\\n, json_extract(record, \'$.x\') FROM t";\n'
                   'const B: &str = "SELECT \\x27--\\x27, json_set(a, \'$.b\', 1) FROM t";\n'
                   'const C: &str = "SELECT \\u{27}json_type(a)\\u{27} FROM t";\n')
        self.assertEqual(self.found(), [("crates/core/src/lib.rs", 1, "json_extract"),
                                        ("crates/core/src/lib.rs", 2, "json_set")])

    def test_an_escape_inside_a_name_reads_as_its_character(self):
        self.write("crates/core/src/lib.rs",
                   'const Q: &str = "SELECT 1,\\n json_extr\\x61ct(record, \'$.x\') FROM t";\n')
        self.assertEqual(self.found(), [("crates/core/src/lib.rs", 1, "json_extract")])

    def test_a_line_continuation_joins_what_it_splits(self):
        self.write("crates/core/src/lib.rs",
                   'const Q: &str = "SELECT json_extr\\\n        act(record, \'$.x\'),\\\n'
                   '    json_set(a, \'$.b\', 1) FROM t";\n')
        self.assertEqual(self.found(), [("crates/core/src/lib.rs", 1, "json_extract"),
                                        ("crates/core/src/lib.rs", 3, "json_set")])

    def test_a_quoted_function_name_is_still_the_function(self):
        self.write("crates/core/tests/queries.sql",
                   "SELECT \"json_extract\"(record, '$.x'), `json_set` (a, '$.b', 1) FROM t;\n"
                   "SELECT [json_object](), \"a--b\", json_type(a) FROM \"json_tree\";\n")
        self.assertEqual(self.found(), [("crates/core/tests/queries.sql", 1, "json_extract"),
                                        ("crates/core/tests/queries.sql", 1, "json_set"),
                                        ("crates/core/tests/queries.sql", 2, "json_object"),
                                        ("crates/core/tests/queries.sql", 2, "json_type")])

    def test_an_operator_is_refused_whatever_its_right_operand(self):
        self.write("crates/core/tests/queries.sql",
                   "SELECT record ->> ('kind'), record->>key, record -> 0 FROM t;\n")
        self.write("crates/core/src/lib.rs",
                   'const Q: &str = "SELECT id FROM t WHERE record->>name=?1";\n'
                   'const P: &str = "{name} -> {port} [{state}]";\n')
        self.assertEqual(self.found(), [("crates/core/src/lib.rs", 1, "->>"),
                                        ("crates/core/tests/queries.sql", 1, "->"),
                                        ("crates/core/tests/queries.sql", 1, "->>"),
                                        ("crates/core/tests/queries.sql", 1, "->>")])

    def test_json_each_reads_only_a_bound_parameter(self):
        self.write("crates/core/src/lib.rs",
                   'const A: &str = "SELECT value FROM json_each(?1)";\n'
                   'const B: &str = "SELECT value FROM json_each( ? )";\n'
                   'const C: &str = "SELECT value FROM t, json_each(t.files)";\n'
                   'const D: &str = "SELECT value FROM t, jsonb_tree(t.record)";\n')
        self.assertEqual(self.found(), [("crates/core/src/lib.rs", 3, "json_each"),
                                        ("crates/core/src/lib.rs", 4, "jsonb_tree")])

    def test_rust_code_comments_and_sql_comments_are_not_sql(self):
        self.write("crates/core/src/lib.rs",
                   "// json_extract(record,'$.x') went away.\n"
                   "fn f(x: &str) -> &str { x }\n"
                   "const Q: &str = \"SELECT 1 -- once json_set(a,'$.b',1)\";\n"
                   "const C: char = '\"'; const P: &str = \"a -> b\";\n")
        self.assertEqual(self.found(), [])

    def test_a_journal_append_takes_a_json_object_source_whose_values_are_still_read(self):
        self.write("crates/core/src/triggers.sql",
                   "SELECT journal_append('linked', NEW.space_id,\n"
                   "    json_object('kind', NEW.kind, 'url', NEW.url));\n"
                   "SELECT journal_append('posted', NEW.id,\n"
                   "    json_object('files', json_array(NEW.file)));\n")
        self.assertEqual(self.found(), [("crates/core/src/triggers.sql", 4, "json_array")])

    def test_the_migrations_are_skipped_and_the_schema_they_generate_is_read(self):
        for path in ("crates/shoal-store/src/migrations.rs",
                     "crates/shoal-store/src/migrations/0300_shapes.sql",
                     "crates/shoal-store/src/migrations/tests.rs",
                     "crates/shoal-store/tests/fixtures/schema_v1.sql"):
            self.write(path, "-- \"\n\"SELECT json_extract(record,'$.x') FROM t\";\n")
        self.write(check.SCHEMA, "CREATE VIEW v AS SELECT json_extract(record,'$.x') FROM t;\n")
        self.assertEqual(self.found(("crates/shoal-store/",)),
                         [(check.SCHEMA, 1, "json_extract")])

    def test_only_a_package_s_src_and_tests_are_read(self):
        self.write("crates/core/benches/load.rs", "const Q: &str = \"SELECT json_object()\";\n")
        self.write("src/main.rs", "const Q: &str = \"SELECT json_object()\";\n")
        self.assertEqual(self.found(("crates/core/", "src/", "tests/")),
                         [("src/main.rs", 1, "json_object")])


if __name__ == "__main__":
    unittest.main()
