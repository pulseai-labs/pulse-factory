# Third-party notices

The Pulse Factory app bundles two typefaces and a set of icons. Each is under its own licence, and the
full text of each licence is in the `licenses/` folder beside this file. The built bundle and the Mac app
made from it carry both.

## Geist and Geist Mono (SIL Open Font License 1.1)

- **What:** the typefaces Geist and Geist Mono, as the variable-weight `woff2` files of the npm packages
  `@fontsource-variable/geist` 5.3.0 and `@fontsource-variable/geist-mono` 5.3.0. The app bundles them and
  fetches nothing at run time.
- **Copyright:** Copyright 2024 The Geist Project Authors (https://github.com/vercel/geist-font).
- **Licence:** SIL Open Font License, Version 1.1. The text is in `licenses/geist-LICENSE.txt` (Geist) and
  `licenses/geist-mono-LICENSE.txt` (Geist Mono). The font files are not modified, and they are not sold on
  their own.

## Lucide (ISC License)

- **What:** the drawing data (paths and shapes) of 31 icons from Lucide (https://lucide.dev), copied from
  `lucide-react` 1.47.0 into `src/ui/icon-nodes.ts` of the app. The app depends on no icon package.
- **Copyright:** Copyright (c) 2026 Lucide Icons and Contributors.
- **Licence:** ISC License. Some Lucide icons are derived from the Feather project and carry the MIT
  License of Cole Bemis (Copyright (c) 2013-present Cole Bemis). Both texts are in
  `licenses/lucide-LICENSE.txt`.
