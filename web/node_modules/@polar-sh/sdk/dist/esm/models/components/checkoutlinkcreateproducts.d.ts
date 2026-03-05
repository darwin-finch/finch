import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CheckoutLinkCreateProductsMetadata = string | number | boolean;
/**
 * Schema to create a new checkout link.
 */
export type CheckoutLinkCreateProducts = {
    /**
     * Key-value object allowing you to store additional information.
     *
     * @remarks
     *
     * The key must be a string with a maximum length of **40 characters**.
     * The value must be either:
     *
     * * A string with a maximum length of **500 characters**
     * * An integer
     * * A boolean
     *
     * You can store up to **50 key-value pairs**.
     */
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    /**
     * Payment processor to use. Currently only Stripe is supported.
     */
    paymentProcessor?: "stripe" | undefined;
    /**
     * Optional label to distinguish links internally
     */
    label?: string | null | undefined;
    /**
     * Whether to allow the customer to apply discount codes. If you apply a discount through `discount_id`, it'll still be applied, but the customer won't be able to change it.
     */
    allowDiscountCodes?: boolean | undefined;
    /**
     * ID of the discount to apply to the checkout. If the discount is not applicable anymore when opening the checkout link, it'll be ignored.
     */
    discountId?: string | null | undefined;
    /**
     * URL where the customer will be redirected after a successful payment.You can add the `checkout_id={CHECKOUT_ID}` query parameter to retrieve the checkout session id.
     */
    successUrl?: string | null | undefined;
    /**
     * List of products that will be available to select at checkout.
     */
    products: Array<string>;
};
/** @internal */
export declare const CheckoutLinkCreateProductsMetadata$inboundSchema: z.ZodType<CheckoutLinkCreateProductsMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkCreateProductsMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CheckoutLinkCreateProductsMetadata$outboundSchema: z.ZodType<CheckoutLinkCreateProductsMetadata$Outbound, z.ZodTypeDef, CheckoutLinkCreateProductsMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkCreateProductsMetadata$ {
    /** @deprecated use `CheckoutLinkCreateProductsMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkCreateProductsMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkCreateProductsMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkCreateProductsMetadata$Outbound, z.ZodTypeDef, CheckoutLinkCreateProductsMetadata>;
    /** @deprecated use `CheckoutLinkCreateProductsMetadata$Outbound` instead. */
    type Outbound = CheckoutLinkCreateProductsMetadata$Outbound;
}
export declare function checkoutLinkCreateProductsMetadataToJSON(checkoutLinkCreateProductsMetadata: CheckoutLinkCreateProductsMetadata): string;
export declare function checkoutLinkCreateProductsMetadataFromJSON(jsonString: string): SafeParseResult<CheckoutLinkCreateProductsMetadata, SDKValidationError>;
/** @internal */
export declare const CheckoutLinkCreateProducts$inboundSchema: z.ZodType<CheckoutLinkCreateProducts, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkCreateProducts$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    payment_processor: "stripe";
    label?: string | null | undefined;
    allow_discount_codes: boolean;
    discount_id?: string | null | undefined;
    success_url?: string | null | undefined;
    products: Array<string>;
};
/** @internal */
export declare const CheckoutLinkCreateProducts$outboundSchema: z.ZodType<CheckoutLinkCreateProducts$Outbound, z.ZodTypeDef, CheckoutLinkCreateProducts>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkCreateProducts$ {
    /** @deprecated use `CheckoutLinkCreateProducts$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkCreateProducts, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkCreateProducts$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkCreateProducts$Outbound, z.ZodTypeDef, CheckoutLinkCreateProducts>;
    /** @deprecated use `CheckoutLinkCreateProducts$Outbound` instead. */
    type Outbound = CheckoutLinkCreateProducts$Outbound;
}
export declare function checkoutLinkCreateProductsToJSON(checkoutLinkCreateProducts: CheckoutLinkCreateProducts): string;
export declare function checkoutLinkCreateProductsFromJSON(jsonString: string): SafeParseResult<CheckoutLinkCreateProducts, SDKValidationError>;
//# sourceMappingURL=checkoutlinkcreateproducts.d.ts.map