# PR UI/UX Checks

Apply these when the PR includes frontend changes (React, CSS, HTML).

## Theme & User Preferences
- [ ] Works correctly in dark mode (colors, borders, shadows, backgrounds)
- [ ] Works correctly in light mode
- [ ] Respects user's font size preference (uses relative units, not hardcoded px)
- [ ] Respects user's color scheme preference (`prefers-color-scheme`)
- [ ] Respects reduced motion preference (`prefers-reduced-motion`)
- [ ] Uses theme tokens/variables, not hardcoded color values
- [ ] Contrast ratios meet WCAG AA (4.5:1 for text, 3:1 for large text)

## Responsive Design
- [ ] Layout works on mobile (320px+)
- [ ] Layout works on tablet (768px+)
- [ ] Layout works on desktop (1024px+)
- [ ] No horizontal scrollbar at any breakpoint
- [ ] Touch targets are large enough (44x44px minimum)
- [ ] Text is readable without zooming on mobile

## Component Patterns
- [ ] Uses existing UI components from the project's component library
- [ ] Doesn't reinvent components that already exist
- [ ] New components follow existing patterns (props, composition, styling)
- [ ] Components are composable and reusable where appropriate

## States & Feedback
- [ ] Loading state shown during async operations
- [ ] Error state shown with clear, user-friendly message
- [ ] Empty state shown when no data exists
- [ ] Success feedback provided for user actions (toast, visual change)
- [ ] Disabled state for buttons during submission
- [ ] Optimistic UI or loading indicators for network calls

## Accessibility
- [ ] Interactive elements reachable via keyboard (Tab, Enter, Escape)
- [ ] Focus order is logical
- [ ] Focus visible indicator present
- [ ] ARIA labels on icon-only buttons
- [ ] Form inputs have associated labels
- [ ] Images have alt text (or aria-hidden if decorative)
- [ ] Screen reader can understand the content flow

## Content & Copy
- [ ] User-facing text is clear and concise
- [ ] Error messages tell the user what to do, not just what went wrong
- [ ] No developer jargon in user-facing text
- [ ] Consistent terminology with rest of app
- [ ] No placeholder text left in (Lorem ipsum, TODO)

## Performance (Frontend)
- [ ] No unnecessary re-renders (React.memo, useMemo, useCallback where needed)
- [ ] Large lists virtualized (if applicable)
- [ ] Images optimized and lazy-loaded (if applicable)
- [ ] No layout shifts (CLS) from dynamic content
- [ ] Bundle size impact reasonable for the feature added
