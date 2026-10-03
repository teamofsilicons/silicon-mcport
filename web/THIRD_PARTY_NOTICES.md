# Third-party notices

Silicon MCPort uses the free, MIT-licensed components of [Arc UI](https://uiarc.dev), from [kuratlielia/arc-library](https://github.com/kuratlielia/arc-library).

The following source was retrieved from the official public registry on October 4, 2026 and installed into `src/components/arc/`:

- `https://uiarc.dev/r/arc-foundation.json`
- `https://uiarc.dev/r/button.json`
- `https://uiarc.dev/r/input.json`
- `https://uiarc.dev/r/dialog.json`
- `https://uiarc.dev/r/switch.json`
- `https://uiarc.dev/r/copy-button.json`
- `https://uiarc.dev/r/segmented-control.json`

The shared foundation and motion tokens are included. Source is vendored without Pro components. MCPort sets its own semantic design tokens and application layouts in `src/styles.css`; Arc component source remains separately identifiable. The upstream MIT license is preserved at `public/UI-ARC-LICENSE.txt` and ships with the website. Copyright and permission notices apply to these source copies.

Other package dependencies and exact versions are recorded in `package-lock.json`. Typography uses DM Sans and Manrope served by Google Fonts, with system fallbacks.
