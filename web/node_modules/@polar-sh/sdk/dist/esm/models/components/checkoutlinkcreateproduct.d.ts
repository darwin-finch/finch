import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CheckoutLinkCreateProductMetadata = string | number | boolean;
/**
 * Schema to create a new checkout link from a a single product.
 *
 * @remarks
 *
 * **Deprecated**: Use `CheckoutLinkCreateProducts` instead.
 */
export type CheckoutLinkCreateProduct = {
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
    productId: string;
};
/** @internal */
export declare const CheckoutLinkCreateProductMetadata$inboundSchema: z.ZodType<CheckoutLinkCreateProductMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkCreateProductMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CheckoutLinkCreateProductMetadata$outboundSchema: z.ZodType<CheckoutLinkCreateProductMetadata$Outbound, z.ZodTypeDef, CheckoutLinkCreateProductMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkCreateProductMetadata$ {
    /** @deprecated use `CheckoutLinkCreateProductMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkCreateProductMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkCreateProductMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkCreateProductMetadata$Outbound, z.ZodTypeDef, CheckoutLinkCreateProductMetadata>;
    /** @deprecated use `CheckoutLinkCreateProductMetadata$Outbound` instead. */
    type Outbound = CheckoutLinkCreateProductMetadata$Outbound;
}
export declare function checkoutLinkCreateProductMetadataToJSON(checkoutLinkCreateProductMetadata: CheckoutLinkCreateProductMetadata): string;
export declare function checkoutLinkCreateProductMetadataFromJSON(jsonString: string): SafeParseResult<CheckoutLinkCreateProductMetadata, SDKValidationError>;
/** @internal */
export declare const CheckoutLinkCreateProduct$inboundSchema: z.ZodType<CheckoutLinkCreateProduct, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkCreateProduct$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    payment_processor: "stripe";
    label?: string | null | undefined;
    allow_discount_codes: boolean;
    discount_id?: string | null | undefined;
    success_url?: string | null | undefined;
    product_id: string;
};
/** @internal */
export declare const CheckoutLinkCreateProduct$outboundSchema: z.ZodType<CheckoutLinkCreateProduct$Outbound, z.ZodTypeDef, CheckoutLinkCreateProduct>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkCreateProduct$ {
    /** @deprecated use `CheckoutLinkCreateProduct$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkCreateProduct, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkCreateProduct$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkCreateProduct$Outbound, z.ZodTypeDef, CheckoutLinkCreateProduct>;
    /** @deprecated use `CheckoutLinkCreateProduct$Outbound` instead. */
    type Outbound = CheckoutLinkCreateProduct$Outbound;
}
export declare function checkoutLinkCreateProductToJSON(checkoutLinkCreateProduct: CheckoutLinkCreateProduct): string;
export declare function checkoutLinkCreateProductFromJSON(jsonString: string): SafeParseResult<CheckoutLinkCreateProduct, SDKValidationError>;
//# sourceMappingURL=checkoutlinkcreateproduct.d.ts.map