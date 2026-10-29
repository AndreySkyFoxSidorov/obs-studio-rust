"""Atomically publish an editable localization CSV; preserve user changes.

Each byte file is staged with create_new, flush/fsync, and same-directory
os.replace. The resource precedes its receipt; a receipt failure after resource
publication is explicit and repairable, not a fictitious two-file transaction.
"""
from pathlib import Path
import argparse
import hashlib
import json
import os
import secrets
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
MAX_CSV_BYTES = 32 * 1024 * 1024
MAX_RECEIPT_BYTES = 64 * 1024


class PackagingError(RuntimeError):
    def __init__(self, phase, detail):
        self.phase = phase
        super().__init__(f"{phase}: {detail}")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def inside(destination, candidate):
    resolved = candidate.resolve(strict=False)
    if not resolved.is_relative_to(destination) or resolved == destination:
        raise ValueError(f"Resource path escapes destination: {candidate} -> {resolved}")
    if candidate.is_symlink():
        raise ValueError(f"Resource file must not be a symbolic link: {candidate}")
    return resolved


def read_bounded(path, limit):
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"File exceeds {limit} bytes: {path}")
    return data


def stage_bytes(destination, target, data):
    parent = inside(destination, target).parent
    temporary = parent / f".{target.name}.{secrets.token_hex(12)}.tmp"
    created = False
    try:
        with temporary.open("xb") as stream:
            created = True
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        return temporary
    except BaseException:
        if created:
            temporary.unlink(missing_ok=True)
        raise


def publish(destination, source, replace_localization=False):
    destination = destination.resolve(strict=True)
    if not destination.is_dir():
        raise PackagingError("before_resource_publication", "Destination must be a directory")
    resource_temporary = receipt_temporary = None
    resource_published = False
    try:
        output = inside(destination, destination / "localization/translations.csv")
        receipt = inside(destination, destination / "localization-resource.json")
        data = read_bounded(source, MAX_CSV_BYTES)
        sha256 = digest(data)
        receipt_bytes, prior = b"", {}
        if receipt.exists():
            try:
                receipt_bytes = read_bounded(receipt, MAX_RECEIPT_BYTES)
                parsed = json.loads(receipt_bytes.decode("utf-8"))
                if isinstance(parsed, dict) and parsed.get("schema") == 1:
                    prior = parsed
            except (ValueError, UnicodeError):
                pass  # Corrupt ownership evidence cannot authorize replacing edits.
        current = None
        if output.exists():
            if not output.is_file():
                raise ValueError(f"Resource destination is not a file: {output}")
            if output.stat().st_size > MAX_CSV_BYTES and not replace_localization:
                return dict(status="preserved_user_edit", path=str(output))
            if not replace_localization or output.stat().st_size <= MAX_CSV_BYTES:
                current = read_bounded(output, MAX_CSV_BYTES)
            if current != data and not replace_localization and digest(current) != prior.get("sha256"):
                return dict(status="preserved_user_edit", path=str(output))
        encoded_receipt = (json.dumps(dict(schema=1, sha256=sha256, source="localization/translations.csv"), indent=2) + "\n").encode("utf-8")
        resource_changed, receipt_changed = current != data, receipt_bytes != encoded_receipt
        if not resource_changed and not receipt_changed:
            return dict(status="unchanged", path=str(output), sha256=sha256)
        output.parent.mkdir(parents=True, exist_ok=True)
        # Prepare both files durably before any previously published CSV changes.
        if resource_changed:
            resource_temporary = stage_bytes(destination, output, data)
        if receipt_changed:
            receipt_temporary = stage_bytes(destination, receipt, encoded_receipt)
        if resource_temporary is not None:
            inside(destination, output)
            os.replace(resource_temporary, output)
            resource_temporary = None
        resource_published = True
        if receipt_temporary is not None:
            inside(destination, receipt)
            os.replace(receipt_temporary, receipt)
            receipt_temporary = None
        return dict(status="copied" if resource_changed else "receipt_repaired", path=str(output), sha256=sha256)
    except (OSError, ValueError) as error:
        raise PackagingError("after_resource_publication" if resource_published else "before_resource_publication", str(error)) from error
    finally:
        for temporary in (resource_temporary, receipt_temporary):
            if temporary is not None:
                temporary.unlink(missing_ok=True)


def publish_browser_helper(destination):
    """Keep an installed browser helper in step with this build when owned.

    CEF itself remains separately provisioned; never replace a custom helper
    without a receipt proving that the previous file was ours.
    """
    destination = destination.resolve(strict=True)
    browser = destination / "browser"
    if not browser.exists():
        return dict(status="runtime_not_installed")
    output = inside(destination, browser / "skylive-browser-helper.exe")
    source = inside(destination, destination / "skylive-browser-helper.exe")
    receipt = inside(destination, destination / "browser-helper-resource.json")
    temporary = receipt_temporary = None
    published = False
    try:
        if not browser.is_dir() or not source.is_file():
            raise ValueError("Browser directory and built helper must exist")
        data = read_bounded(source, 32 * 1024 * 1024)
        if not data:
            raise ValueError("Built browser helper is empty")
        old_receipt = {}
        if receipt.exists():
            try:
                value = json.loads(read_bounded(receipt, MAX_RECEIPT_BYTES).decode("utf-8"))
                if isinstance(value, dict) and value.get("schema") == 1:
                    old_receipt = value
            except (ValueError, UnicodeError):
                pass
        current = read_bounded(output, 32 * 1024 * 1024) if output.exists() else None
        owned = {old_receipt.get("sha256"), old_receipt.get("previous_sha256")}
        if current is not None and current != data and digest(current) not in owned:
            return dict(status="preserved_unowned_helper", path=str(output))
        checksum = digest(data)
        receipt_data = (json.dumps(dict(schema=1, sha256=checksum, previous_sha256=digest(current) if current is not None else None, source="skylive-browser-helper.exe"), indent=2) + "\n").encode("utf-8")
        existing_receipt = read_bounded(receipt, MAX_RECEIPT_BYTES) if receipt.exists() else None
        if current != data:
            temporary = stage_bytes(destination, output, data)
        if existing_receipt != receipt_data:
            receipt_temporary = stage_bytes(destination, receipt, receipt_data)
        # The receipt identifies both the old and new helper, so interruption
        # between these two replacements remains recoverable on a later run.
        if receipt_temporary is not None:
            os.replace(receipt_temporary, receipt)
            receipt_temporary = None
        if temporary is not None:
            os.replace(temporary, output)
            temporary = None
            published = True
        return dict(status="copied" if published else "unchanged", path=str(output), sha256=checksum)
    except (OSError, ValueError) as error:
        raise PackagingError("after_browser_helper_publication" if published else "before_browser_helper_publication", str(error)) from error
    finally:
        for path in (temporary, receipt_temporary):
            if path is not None:
                path.unlink(missing_ok=True)


def publish_codec_notices(destination, root=ROOT):
    """Authoritative source licenses, individually atomic; receipt published last.

    This is not a twelve-file transaction. A mid-publication failure preserves
    the old receipt and reports which phase failed; rerunning repairs the set.
    Editable localization ownership policy is independent of required notices.
    """
    destination = destination.resolve(strict=True)
    prepared = []
    receipt_rows = []
    staged = []
    published = False
    try:
        if not destination.is_dir():
            raise ValueError("Destination must be a directory")
        for folder in ("codec-bits", "h264", "aac", "mp4"):
            package = "skylive-" + folder
            for name in ("LICENSE-MIT", "LICENSE-APACHE", "NOTICE"):
                source = root / "crates" / folder / name
                data = read_bounded(source, 64 * 1024)
                if not data:
                    raise ValueError(f"Empty required codec notice: {source}")
                target = inside(destination, destination / "licenses" / package / name)
                prepared.append((target, data))
                receipt_rows.append(dict(package=package, path=target.relative_to(destination).as_posix(),
                                         sha256=digest(data), license="MIT OR Apache-2.0", selected_license="MIT"))
        receipt = inside(destination, destination / "codec-notices.json")
        encoded = (json.dumps(dict(schema=1, files=receipt_rows), indent=2) + "\n").encode("utf-8")
        for target, data in prepared:
            if target.exists() and read_bounded(target, 64 * 1024) == data:
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            staged.append((target, stage_bytes(destination, target, data)))
        for target, temporary in staged:
            inside(destination, target)
            os.replace(temporary, target)
            published = True
        if not receipt.exists() or read_bounded(receipt, MAX_RECEIPT_BYTES) != encoded:
            temporary = stage_bytes(destination, receipt, encoded)
            staged.append((receipt, temporary))
            inside(destination, receipt)
            os.replace(temporary, receipt)
        return dict(status="published" if published else "unchanged", files=len(prepared), receipt=str(receipt))
    except (OSError, ValueError) as error:
        raise PackagingError("after_codec_notice_publication" if published else "before_codec_notice_publication", str(error)) from error
    finally:
        for _, temporary in staged:
            temporary.unlink(missing_ok=True)


def publish_font_notices(destination, root=ROOT, application=False):
    """Publish the embedded font's actual license and pinned source manifest.

    Stage both before replacement. Each replacement is atomic; a partial publish
    is reported and a repeat repairs it. Never alter an editable CSV here.
    """
    destination = destination.resolve(strict=True)
    staged = []
    published = False
    try:
        prepared = []
        files = [(root / "assets/fonts" / name, "licenses/fonts/" + name)
                 for name in ("OFL.txt", "manifest.json")]
        if application:
            files += [(root / "LICENSE", "LICENSE"),
                      (root / "assets/licenses/THIRD-PARTY-NOTICES.md", "licenses/THIRD-PARTY-NOTICES.md")]
            files += [(root / "crates/media/src/craft_audio" / source, "licenses/craft/" + target)
                      for source, target in (("LICENSE-MIT", "filmcraft-MIT.txt"),
                                             ("NOTICE", "filmcraft-NOTICE.txt"),
                                             ("SOURCE.json", "filmcraft-source.json"))]
            files += [(root / "assets/licenses/craft" / name, "licenses/craft/" + name)
                      for name in ("effectcraft-MIT.txt", "photocraft-MIT.txt", "references.json")]
        if application:
            files += [(root / "crates/person-matte/models" / name, "licenses/person-matte/" + name)
                      for name in ("NOTICE", "selfie-segmentation.LICENSE", "Apache-2.0.txt", "manifest.json")]
            files += [(root / "vendor/rten-0.21.0" / name, "licenses/rten/" + name)
                      for name in ("Cargo.toml", "LICENSE-MIT.txt", "LICENSE-Apache-2.0.txt", "SKYLIVE-PATCH.json")]
        receipt = []
        for source, relative in files:
            data = read_bounded(source, 64 * 1024)
            if not data:
                raise ValueError(f"Empty required notice: {source}")
            prepared.append((inside(destination, destination / relative), data))
            receipt.append(dict(path=relative, bytes=len(data), sha256=digest(data)))
        if application:
            data=(json.dumps(dict(scope="Project license, selected Craft transfers, fixed Selfie model, RTen runtime and embedded Noto fonts",files=receipt),indent=2)+"\n").encode("utf-8")
            prepared.append((inside(destination,destination/"licenses/manifest.json"),data))
        for target, data in prepared:
            if target.exists() and read_bounded(target, 64 * 1024) == data:
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            staged.append((target, stage_bytes(destination, target, data)))
        for target, temporary in staged:
            inside(destination, target)
            os.replace(temporary, target)
            published = True
        return dict(status="published" if published else "unchanged", files=len(prepared))
    except (OSError, ValueError) as error:
        raise PackagingError("after_font_notice_publication" if published else "before_font_notice_publication", str(error)) from error
    finally:
        for _, temporary in staged:
            temporary.unlink(missing_ok=True)


class Tests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.destination = self.root / "app"
        self.destination.mkdir()
        self.source = self.root / "source.csv"
        self.source.write_bytes(b'key,en-US\r\nA,"First"\r\n')
        self.output = self.destination / "localization/translations.csv"
        self.receipt = self.destination / "localization-resource.json"

    def package(self, replace=False):
        return publish(self.destination, self.source, replace)

    def files(self):
        return {p.relative_to(self.destination).as_posix(): p.read_bytes() for p in self.destination.rglob("*") if p.is_file()}

    def upgrade(self):
        self.package()
        before = self.files()
        self.source.write_bytes(b'key,en-US\r\nA,"Second"\r\n')
        return before

    def test_first_copy_and_idempotence(self):
        self.assertEqual(self.package()["status"], "copied")
        self.assertEqual(self.output.read_bytes(), self.source.read_bytes())
        self.assertEqual(json.loads(self.receipt.read_bytes())["sha256"], digest(self.source.read_bytes()))
        before = self.files()
        self.assertEqual(set(before), {"localization/translations.csv", "localization-resource.json"})
        with mock.patch.object(os, "replace", side_effect=AssertionError("No rewrite")):
            self.assertEqual(self.package()["status"], "unchanged")
        self.assertEqual(self.files(), before)

    def test_unedited_resource_upgrade(self):
        self.upgrade()
        self.assertEqual(self.package()["status"], "copied")
        self.assertEqual(self.output.read_bytes(), self.source.read_bytes())

    def test_edit_preserved_and_explicit_replace(self):
        self.upgrade()
        self.output.write_bytes(b"custom edit")
        before = self.files()
        self.assertEqual(self.package()["status"], "preserved_user_edit")
        self.assertEqual(self.files(), before)
        self.assertEqual(self.package(True)["status"], "copied")
        self.assertEqual(self.output.read_bytes(), self.source.read_bytes())

    def test_corrupt_receipt_does_not_authorize_overwrite(self):
        self.package()
        self.output.write_bytes(b"custom edit")
        self.receipt.write_bytes(b"not JSON")
        before = self.files()
        self.assertEqual(self.package()["status"], "preserved_user_edit")
        self.assertEqual(self.files(), before)

    def test_resource_replace_failure_retains_prior_files(self):
        before = self.upgrade()
        with mock.patch.object(os, "replace", side_effect=OSError("fixture replace failure")):
            with self.assertRaises(PackagingError) as caught:
                self.package()
        self.assertEqual(caught.exception.phase, "before_resource_publication")
        self.assertEqual(self.files(), before)

    def test_receipt_stage_failure_retains_prior_files(self):
        before = self.upgrade()
        original = stage_bytes
        def fail_receipt(destination, target, data):
            if target == self.receipt:
                raise OSError("fixture receipt staging failure")
            return original(destination, target, data)
        with mock.patch(__name__ + ".stage_bytes", side_effect=fail_receipt):
            with self.assertRaises(PackagingError) as caught:
                self.package()
        self.assertEqual(caught.exception.phase, "before_resource_publication")
        self.assertEqual(self.files(), before)

    def test_fsync_failure_retains_prior_files(self):
        before = self.upgrade()
        with mock.patch.object(os, "fsync", side_effect=OSError("fixture fsync failure")):
            with self.assertRaises(PackagingError):
                self.package()
        self.assertEqual(self.files(), before)

    def test_receipt_failure_after_resource_is_repairable(self):
        before = self.upgrade()
        original = os.replace
        def fail_receipt(source, target):
            if Path(target) == self.receipt:
                raise OSError("fixture receipt publication failure")
            return original(source, target)
        with mock.patch.object(os, "replace", side_effect=fail_receipt):
            with self.assertRaises(PackagingError) as caught:
                self.package()
        self.assertEqual(caught.exception.phase, "after_resource_publication")
        self.assertEqual(self.output.read_bytes(), self.source.read_bytes())
        self.assertEqual(self.receipt.read_bytes(), before["localization-resource.json"])
        self.assertEqual(set(self.files()), set(before))
        self.assertEqual(self.package()["status"], "receipt_repaired")

    def test_create_new_collision_preserves_another_writers_file(self):
        self.output.parent.mkdir()
        collision = self.output.parent / ".translations.csv.collision.tmp"
        collision.write_bytes(b"another writer")
        with mock.patch.object(secrets, "token_hex", return_value="collision"):
            with self.assertRaises(FileExistsError):
                stage_bytes(self.destination, self.output, b"new")
        self.assertEqual(collision.read_bytes(), b"another writer")
        self.assertEqual(set(self.files()), {"localization/.translations.csv.collision.tmp"})

    def test_resolved_output_cannot_escape(self):
        with self.assertRaises(ValueError):
            inside(self.destination, self.destination / "../outside.csv")
        self.assertEqual(self.files(), {})

    def fake_codec_sources(self):
        for folder in ("codec-bits", "h264", "aac", "mp4"):
            source=self.root/"crates"/folder
            source.mkdir(parents=True)
            for name in ("LICENSE-MIT", "LICENSE-APACHE", "NOTICE"):
                (source/name).write_bytes((folder+":"+name).encode())

    def test_codec_notices_complete_idempotent_and_receipt_hashes_match(self):
        self.fake_codec_sources()
        self.assertEqual(publish_codec_notices(self.destination,self.root)["files"],12)
        before=self.files()
        self.assertEqual(publish_codec_notices(self.destination,self.root)["status"],"unchanged")
        self.assertEqual(before,self.files())
        receipt=json.loads((self.destination/"codec-notices.json").read_text())
        for row in receipt["files"]:
            self.assertEqual(row["sha256"],digest((self.destination/row["path"]).read_bytes()))
            self.assertEqual(row["selected_license"],"MIT")

    def test_codec_notice_preparation_failure_keeps_prior_complete_set(self):
        self.fake_codec_sources();publish_codec_notices(self.destination,self.root)
        before=self.files()
        (self.root/"crates/h264/LICENSE-MIT").unlink()
        with self.assertRaises(PackagingError) as caught:
            publish_codec_notices(self.destination,self.root)
        self.assertEqual(caught.exception.phase,"before_codec_notice_publication")
        self.assertEqual(before,self.files())

    def test_codec_notice_staging_failure_keeps_prior_bytes_and_no_temps(self):
        self.fake_codec_sources();publish_codec_notices(self.destination,self.root)
        before=self.files();(self.root/"crates/h264/NOTICE").write_bytes(b"updated notice")
        with mock.patch.object(os,"fsync",side_effect=OSError("injected disk error")):
            with self.assertRaises(PackagingError):publish_codec_notices(self.destination,self.root)
        self.assertEqual(before,self.files())

    def test_font_notice_update_is_idempotent_and_failed_preparation_preserves_old_set(self):
        source=self.root/"assets/fonts";source.mkdir(parents=True)
        (source/"OFL.txt").write_bytes(b"fixture license")
        (source/"manifest.json").write_bytes(b'{"files":[]}')
        self.assertEqual(publish_font_notices(self.destination,self.root)["status"],"published")
        before=self.files()
        self.assertEqual(publish_font_notices(self.destination,self.root)["status"],"unchanged")
        (source/"manifest.json").unlink()
        with self.assertRaises(PackagingError):publish_font_notices(self.destination,self.root)
        self.assertEqual(before,self.files())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path)
    parser.add_argument("--replace-localization", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.main(argv=[sys.argv[0]], exit=False)
        raise SystemExit(0 if result.result.wasSuccessful() else 1)
    if args.destination is None:
        parser.error("--destination is required except with --self-test")
    try:
        result = publish(args.destination, ROOT / "localization/translations.csv", args.replace_localization)
        result["browser_helper"] = publish_browser_helper(args.destination)
        result["codec_notices"] = publish_codec_notices(args.destination)
        result["application_notices"] = publish_font_notices(args.destination, application=True)
    except (OSError, PackagingError) as error:
        print(json.dumps(dict(status="error", phase=getattr(error, "phase", "before_resource_publication"), detail=str(error))))
        raise SystemExit(1)
    print(json.dumps(result))


if __name__ == "__main__":
    main()
