import unittest

import check_crate_dependencies as check
from test_affected_packages import PACKAGES, package

METADATA = {
    "workspace_root": "/work",
    "workspace_members": [package["id"] for package in PACKAGES],
    "packages": PACKAGES + [package("serde", "vendor/serde")],
}
DECLARED = check.declared_dependencies(METADATA)
ALLOWED = {
    "shoal": {"dependencies": ["cli", "daemon"], "dev-dependencies": ["core"]},
    "cli": {"dependencies": ["core"]},
    "daemon": {"dependencies": ["core"]},
    "core": {"dependencies": ["time"]},
    "time": {"dependencies": []},
    "proxy": {"dependencies": [], "dev-dependencies": ["time"]},
}


def packages(problems):
    return [package for package, _problem in problems]


class CheckCrateDependenciesTests(unittest.TestCase):
    def test_reads_each_members_dependencies_on_members_with_their_kind(self):
        self.assertEqual(DECLARED["shoal"],
                         [("cli", "normal"), ("core", "dev"), ("daemon", "normal")])
        self.assertEqual(DECLARED["time"], [])
        self.assertNotIn("serde", DECLARED)

    def test_dependencies_the_file_allows_pass(self):
        self.assertEqual(check.problems(ALLOWED, DECLARED), [])

    def test_a_dependency_outside_the_entry_fails(self):
        allowed = {**ALLOWED, "cli": {"dependencies": []}}
        self.assertEqual(packages(check.problems(allowed, DECLARED)), ["cli"])

    def test_a_dev_only_entry_allows_no_normal_or_build_dependency(self):
        declared = {**DECLARED, "core": [("time", "build")]}
        allowed = {**ALLOWED, "core": {"dependencies": [], "dev-dependencies": ["time"]}}
        self.assertEqual(packages(check.problems(allowed, declared)), ["core"])

    def test_a_package_without_an_entry_fails(self):
        allowed = {name: entry for name, entry in ALLOWED.items() if name != "proxy"}
        self.assertEqual(packages(check.problems(allowed, DECLARED)), ["proxy"])

    def test_an_entry_for_no_package_fails(self):
        allowed = {**ALLOWED, "gone": {"dependencies": []}}
        self.assertEqual(packages(check.problems(allowed, DECLARED)), ["gone"])

    def test_an_entry_naming_no_package_or_an_unknown_key_fails(self):
        allowed = {**ALLOWED, "time": {"dependencies": ["gone"], "dev_dependencies": []}}
        self.assertEqual(packages(check.problems(allowed, DECLARED)), ["time", "time"])


if __name__ == "__main__":
    unittest.main()
