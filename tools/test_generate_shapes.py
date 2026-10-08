"""Portable synthetic checks; no Minecraft files, Java, or network required."""
from contextlib import contextmanager
import hashlib
import io
import json
from pathlib import Path
import shutil
import tempfile
import types
import unittest
from unittest import mock
import uuid
import zipfile

import generate_shapes as helper


@contextmanager
def scratch():
    # Inherit directory ACLs instead of relying on platform-specific private
    # TemporaryDirectory permissions. Remove only this exact created child.
    parent = Path(tempfile.gettempdir()).resolve()
    path = parent / ("leafish-shape-helper-" + uuid.uuid4().hex)
    path.mkdir()
    try:
        yield path
    finally:
        if not path.resolve().is_relative_to(parent) or path.is_symlink():
            raise RuntimeError("Scratch path changed; refusing cleanup")
        shutil.rmtree(path)


def digest(data):
    return hashlib.sha256(data).hexdigest()


class ShapeHelperTests(unittest.TestCase):
    def test_bundle_paths_reject_traversal_absolute_and_platform_aliases(self):
        self.assertEqual(str(helper.safe_relative("org/example/library.jar")), "org/example/library.jar")
        for value in ["", "/root.jar", "../bad.jar", "a/../b.jar", "a/./b.jar", "a//b.jar",
                      "C:/file.jar", "a\\b.jar", "a/"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                helper.safe_relative(value)

    def test_bundle_lists_reject_bad_hashes_duplicates_and_paths(self):
        valid = digest(b"payload") + "\ttest\tvalid.jar"
        for text in ["not-a-hash\ttest\tx.jar", valid + "\n" + valid,
                     digest(b"x") + "\ttest\t../x.jar", ""]:
            with self.subTest(text=text[:20]):
                buffer = io.BytesIO()
                with zipfile.ZipFile(buffer, "w") as jar:
                    jar.writestr("META-INF/libraries.list", text)
                with zipfile.ZipFile(io.BytesIO(buffer.getvalue())) as jar, self.assertRaises(ValueError):
                    helper.bundle_list(jar, "libraries")

    def archive(self, path, *, bad_library=False):
        inner, library = b"synthetic inner", b"synthetic dependency"
        with zipfile.ZipFile(path, "w") as jar:
            jar.writestr("version.json", json.dumps({"id": "1.21.1", "protocol_version": 767}))
            jar.writestr("META-INF/versions.list", digest(inner) + "\t1.21.1\t1.21.1/server.jar")
            jar.writestr("META-INF/libraries.list", digest(library) + "\tsynthetic\torg/test.jar")
            jar.writestr("META-INF/versions/1.21.1/server.jar", inner)
            jar.writestr("META-INF/libraries/org/test.jar", b"changed" if bad_library else library)
            jar.writestr("unused/never-extracted.txt", b"not listed")
        return inner, library

    def test_extraction_copies_only_listed_verified_files(self):
        with scratch() as root:
            archive = root / "bundle.jar"
            inner, library = self.archive(archive)
            output = root / "new"; output.mkdir()
            with mock.patch.object(helper, "INNER_SHA256", digest(inner)):
                server, libraries = helper.extract_reference(archive, output)
            self.assertEqual(server.read_bytes(), inner)
            self.assertEqual([p.read_bytes() for p in libraries], [library])
            self.assertEqual(len([p for p in output.rglob("*") if p.is_file()]), 2)

    def test_extraction_rejects_digest_mismatch(self):
        with scratch() as root:
            archive = root / "bundle.jar"
            inner, _ = self.archive(archive, bad_library=True)
            output = root / "new"; output.mkdir()
            with mock.patch.object(helper, "INNER_SHA256", digest(inner)), self.assertRaisesRegex(ValueError, "digest mismatch"):
                helper.extract_reference(archive, output)

    def test_inputs_must_match_both_size_limit_and_digest(self):
        with scratch() as root:
            path = root / "input"; path.write_bytes(b"data")
            self.assertEqual(helper.checked_input(path, digest(b"data"), "test", 4), path)
            with self.assertRaises(ValueError): helper.checked_input(path, digest(b"other"), "test", 4)
            with self.assertRaises(ValueError): helper.checked_input(path, digest(b"data"), "test", 3)

    def test_existing_output_rejected_before_java_or_extraction(self):
        with scratch() as root:
            marker = root / "keep.txt"; marker.write_text("unchanged")
            args = types.SimpleNamespace(server_jar=marker, mappings=marker, blocks=marker,
                                         output=root, java="java", javac="javac")
            with mock.patch.object(helper, "checked_input", return_value=marker), \
                 mock.patch.object(helper, "extract_reference") as extract, \
                 mock.patch.object(helper.subprocess, "run") as execute:
                with self.assertRaisesRegex(ValueError, "must be new"): helper.run(args)
                extract.assert_not_called(); execute.assert_not_called()
            self.assertEqual(marker.read_text(), "unchanged")


if __name__ == "__main__":
    unittest.main()
