import { Slot } from '@radix-ui/react-slot';
import { cva, type VariantProps } from 'class-variance-authority';
import type * as React from 'react';

import { cn } from '@/lib/utils';

const buttonVariants = cva(
  [
    'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-lg text-sm',
    'font-medium transition-all duration-150 disabled:pointer-events-none disabled:opacity-50',
    "[&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 shrink-0",
    '[&_svg]:shrink-0 outline-none focus-visible:ring-2',
    'focus-visible:ring-[var(--border-focus)]',
  ],
  {
    variants: {
      variant: {
        default: [
          'bg-[var(--accent-cyan)] text-[var(--surface-base)]',
          'hover:bg-[var(--accent-cyan)]/90',
        ],
        destructive: [
          'bg-[var(--destructive)] text-[var(--destructive-foreground)]',
          'hover:opacity-90',
        ],
        outline: [
          'border border-[var(--border-default)] bg-transparent',
          'hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]',
        ],
        secondary: [
          'bg-[var(--surface-hover)] text-[var(--text-secondary)]',
          'hover:text-[var(--text-primary)]',
        ],
        ghost: [
          'hover:bg-[var(--surface-hover)] text-[var(--text-muted)]',
          'hover:text-[var(--text-primary)]',
        ],
        link: 'text-[var(--accent-cyan)] underline-offset-4 hover:underline',
      },
      size: {
        default: 'h-11 px-5',
        sm: 'h-9 px-4 text-sm',
        lg: 'h-12 px-6',
        icon: 'size-11',
        'icon-sm': 'size-9',
        'icon-lg': 'size-12',
      },
    },
    defaultVariants: {
      variant: 'default',
      size: 'default',
    },
  }
);

function Button({
  className,
  variant = 'default',
  size = 'default',
  asChild = false,
  ...props
}: React.ComponentProps<'button'> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean;
  }) {
  const Comp = asChild ? Slot : 'button';

  return (
    <Comp
      data-slot="button"
      data-variant={variant}
      data-size={size}
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  );
}

export { Button, buttonVariants };
