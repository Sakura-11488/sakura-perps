// Test shell only. Application navigation is verified in the full Expo export.
import { cloneElement, isValidElement, type AnchorHTMLAttributes, type ReactElement } from 'react';
export function Link({ asChild, ...props }: AnchorHTMLAttributes<HTMLAnchorElement> & { asChild?: boolean }) {
  if (asChild && isValidElement(props.children)) {
    return cloneElement(props.children as ReactElement<AnchorHTMLAttributes<HTMLAnchorElement>>, { href: props.href });
  }
  return <a {...props} />;
}
