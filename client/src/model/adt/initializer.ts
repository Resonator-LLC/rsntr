// Attributes for elements about to be created (`Initializer`).
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased` (`Initializer`,
// `CompoundInitializer`) of Apache Wave, licensed under the Apache License,
// Version 2.0.

import { AdtError } from "./error";

/** The attributes an element is created with. */
export type InitialAttributes = Map<string, string>;

/** Contributes attributes to an element about to be created. */
export type Initializer = (target: InitialAttributes) => void;

/**
 * Adds an attribute; throws {@link AdtError} `duplicateInitialiserKey` if
 * the target already has one with that name
 * (`Initializer.Helper.initialiseAttribute`).
 */
export function initialiseAttribute(target: InitialAttributes, key: string, value: string): void {
  if (target.has(key)) {
    throw AdtError.duplicateInitialiserKey(key);
  }
  target.set(key, value);
}

/**
 * The attributes an initializer produces
 * (`Initializer.Helper.buildAttributes`).
 */
export function buildAttributes(initializer: Initializer | CompoundInitializer): InitialAttributes {
  const attributes: InitialAttributes = new Map();
  if (initializer instanceof CompoundInitializer) {
    initializer.initialize(attributes);
  } else {
    initializer(attributes);
  }
  return attributes;
}

/** Several initializers applied in order (`CompoundInitializer`). */
export class CompoundInitializer {
  private readonly components: Initializer[] = [];

  /** Adds a component, to be applied after those already added. */
  with(component: Initializer | CompoundInitializer): this {
    this.components.push(
      component instanceof CompoundInitializer ? component.initializer : component,
    );
    return this;
  }

  initialize(target: InitialAttributes): void {
    for (const component of this.components) {
      component(target);
    }
  }

  /** This object as a plain {@link Initializer}. */
  readonly initializer: Initializer = (target) => this.initialize(target);
}
