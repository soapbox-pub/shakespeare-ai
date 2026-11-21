import { lazy } from "react";
import { BrowserRouter, Route, Routes } from "react-router-dom";

import { Layout } from "./components/Layout";
import { ScrollToTop } from "./components/ScrollToTop";
import DocsPage from "./pages/DocsPage";
import HomePage from "./pages/HomePage";
import TermsPage from "./pages/TermsPage";

// The pages behind a login are chunks of their own, so the landing page
// loads only what it shows. The prerendered pages (see prerender.tsx) aren't:
// the app replaces their HTML as it starts, and a chunk still loading then
// would blank the page for a moment.
const CreditsPage = lazy(() => import("./pages/CreditsPage"));
const KeysPage = lazy(() => import("./pages/KeysPage"));
const UsagePage = lazy(() => import("./pages/UsagePage"));
const NotFound = lazy(() => import("./pages/NotFound"));

export function AppRouter() {
  return (
    <BrowserRouter>
      <ScrollToTop />
      <AppRoutes />
    </BrowserRouter>
  );
}

export function AppRoutes() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route path="/" element={<HomePage />} />
        <Route path="/credits" element={<CreditsPage />} />
        <Route path="/keys" element={<KeysPage />} />
        <Route path="/usage" element={<UsagePage />} />
        <Route path="/docs" element={<DocsPage />} />
        <Route path="/terms" element={<TermsPage />} />
        {/* ADD ALL CUSTOM ROUTES ABOVE THE CATCH-ALL "*" ROUTE */}
        <Route path="*" element={<NotFound />} />
      </Route>
    </Routes>
  );
}
export default AppRouter;
