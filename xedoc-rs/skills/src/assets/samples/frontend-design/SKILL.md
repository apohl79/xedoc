---
name: frontend-design
description: Design and implement polished, usable frontend and UI work. Use for applications, websites, games, visual components, layouts, interaction flows, responsive styling, or product design decisions.
---

# Frontend Design

## Build With Empathy

- Follow existing design frameworks and conventions when they are available.
- Consider the audience when choosing features, layout, components, visual style, text, and interaction patterns.
- Tailor the design to the domain. SaaS, CRM, and operational tools should be quiet, utilitarian, information-dense, and work-focused. Games can be expressive, animated, and playful.
- Make common workflows ergonomic, efficient, and comprehensive so users can move naturally between views and pages.

## Design Instructions

- Use icons in buttons for tools, swatches for color, segmented controls for modes, toggles or checkboxes for binary settings, sliders, steppers, or inputs for numbers, menus for option sets, tabs for views, and text or icon-plus-text buttons only for clear commands. Keep cards at an 8px border radius or less unless the design system requires otherwise.
- Prefer familiar symbols or icons to rounded text controls when they are clearer. Add tooltips for unfamiliar icons.
- Use Lucide icons when available, or the application's existing icon library.
- Build the controls, states, and views that the target user would naturally expect.
- Do not use visible in-app text to explain the application's features, styling, visual elements, shortcuts, or operation.
- Build the usable experience as the first screen. Do not create a landing page unless it is required.
- When a hero is required, use a relevant image, generated bitmap, or immersive full-bleed interactive scene behind uncarded text. Do not use split text/media card layouts, gradient or SVG heroes, or SVG hero illustrations where real or generated imagery can carry the subject.
- On branded, product, venue, portfolio, or object-focused pages, make the subject visible in the first viewport. Leave a hint of the next section visible on mobile and desktop.
- For landing-page heroes, make the H1 the brand, product, place, person, offer, or category; keep descriptive value propositions in supporting copy.
- Websites and games need visual assets. Prefer image search, known relevant images, or generated bitmap images over SVGs unless a game needs custom SVG or Three.js assets. Show the actual product, place, object, state, gameplay, or person when users need to inspect it.
- For games or interactive tools with established rules, physics, parsing, or AI engines, use a proven library for the core domain unless the user requests a from-scratch implementation.
- Use Three.js for 3D elements. Keep the primary scene full-bleed or unframed, then verify with Playwright screenshots and canvas-pixel checks across desktop and mobile viewports that it is nonblank, framed correctly, interactive, and renders assets without overlap.
- Do not nest UI cards or style page sections as floating cards. Use cards only for repeated items, modals, and genuinely framed tools; use full-width bands or unframed layouts for page sections.
- Do not add decorative orbs, gradient orbs, or bokeh blobs.
- Ensure text fits its parent UI element on mobile and desktop without overlap. Wrap or size it dynamically when necessary, while keeping buttons and cards polished.
- Match display text to its container: reserve hero-scale type for true heroes and use tighter headings in compact panels, cards, sidebars, dashboards, and tool surfaces.
- Give fixed-format UI elements responsive, stable dimensions with aspect ratios, grid tracks, min/max sizes, or container-relative sizing so dynamic content cannot shift the layout.
- Do not scale font size with viewport width. Keep letter spacing at zero or the existing design-system value.
- Avoid one-note palettes and dominant purple, beige, dark-blue, or brown themes. Scan CSS colors before finalizing and revise when the page reads as a single hue family.
- Ensure UI elements and on-screen text never overlap incoherently.

When a site or app needs a development server, start it after implementation and give the user its URL. If opening HTML directly is sufficient, do not start a server; give the user a link to the HTML file instead.
