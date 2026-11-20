import { lazy } from "react";
import { BrowserRouter, Route, Routes } from "react-router-dom";

import { Layout } from "./components/Layout";
import { ScrollToTop } from "./components/ScrollToTop";
import HomePage from "./pages/HomePage";

// The pages behind a login are chunks of their own, so the landing page
// loads only what it shows.
const CreditsPage = lazy(() => import("./pages/CreditsPage"));
const KeysPage = lazy(() => import("./pages/KeysPage"));
const UsagePage = lazy(() => import("./pages/UsagePage"));
const DocsPage = lazy(() => import("./pages/DocsPage"));
const TermsPage = lazy(() => import("./pages/TermsPage"));
const NotFound = lazy(() => import("./pages/NotFound"));

export function AppRouter() {
  return (
    <BrowserRouter>
      <ScrollToTop />
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
    </BrowserRouter>
  );
}
export default AppRouter;
