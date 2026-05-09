package com.example;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

/**
 * AnnotationType — AC-1.4 fixture.
 *
 * Verifies:
 *  - Annotation type (`@interface`) extracted as SymbolKind::Interface.
 *  - Public access level detected.
 *  - Annotation elements are Function children.
 */
@Retention(RetentionPolicy.RUNTIME)
@Target(ElementType.TYPE)
public @interface MyAnnotation {

    /** The primary annotation value. */
    String value();

    /** A numeric priority with a default. */
    int priority() default 0;

    /** Optional description. */
    String description() default "";
}
