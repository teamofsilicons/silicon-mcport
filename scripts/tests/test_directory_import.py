"""The importer reads discovery links only, with explicit provenance and bounds."""
import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'import-directory.py'
SPEC = importlib.util.spec_from_file_location('directory_import', SCRIPT)
IMPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMPORTER)


class DirectoryImportTests(unittest.TestCase):
    def readme(self, extra=''):
        rows = '\n'.join(f'- **[Server {i}](https://github.com/vendor/server-{i})** - description' for i in range(100))
        return '## Official Servers\n' + rows + '\n' + extra

    def test_excludes_non_server_sections_and_never_infers_executable_setup(self):
        text = self.readme('- **[Runner](https://example.com/docs?tracking=1#install)** - Run `sh -c install.sh`; token stays private.\n## Clients\n- **[Client](https://example.com/client)** - a client\n## Sponsors\n- **[Sponsor](https://example.com/ad)** - ad')
        entries, skipped = IMPORTER.parse(text)
        self.assertEqual(len(entries), 101)
        self.assertEqual(skipped, [])
        runner = next(entry for entry in entries if entry['name'] == 'Runner')
        self.assertEqual(runner['source_url'], 'https://example.com/docs')
        self.assertIsNone(runner['template'])
        self.assertNotIn('command', runner)
        self.assertFalse(any(entry['name'] in ('Client', 'Sponsor') for entry in entries))

    def test_reports_malformed_rows_and_deduplicates_source_identity(self):
        entries, skipped = IMPORTER.parse(self.readme('- malformed server link\n- **[Duplicate](https://github.com/vendor/server-1/#setup)** - duplicate'))
        self.assertEqual(len(entries), 100)
        self.assertEqual(len(skipped), 1)
        self.assertEqual(skipped[0]['line'], 102)
        self.assertEqual(skipped[0]['reason'], 'Unrecognized Markdown server row')

    def test_refuses_credential_sources_and_unexpectedly_empty_imports(self):
        with self.assertRaisesRegex(ValueError, 'Credential-bearing'):
            IMPORTER.parse(self.readme('- **[Bad](https://owner:secret@example.com/docs)** - no'))
        with self.assertRaisesRegex(ValueError, 'Unexpected upstream size'):
            IMPORTER.parse('## Clients\n- **[Client](https://example.com)** - none')


if __name__ == '__main__':
    unittest.main()
