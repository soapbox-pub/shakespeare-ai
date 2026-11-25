import type { SVGProps } from 'react';

/** An M in a circle, for Monero, drawn as lucide draws its icons. */
export function MoneroIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="24"
      height="24"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      {...props}
    >
      <circle cx="12" cy="12" r="10" />
      <path d="M7 16V8l5 5 5-5v8" />
    </svg>
  );
}
