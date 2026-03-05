import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CheckoutLinkCreateProductPriceMetadata = string | number | boolean;
/**
 * Schema to create a new checkout link from a a single product price.
 *
 * @remarks
 *
 * **Deprecated**: Use `CheckoutLinkCreateProducts` instead.
 */
export type CheckoutLinkCreateProductPrice = {
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
    productPriceId: string;
};
/** @internal */
export declare const CheckoutLinkCreateProductPriceMetadata$inboundSchema: z.ZodType<CheckoutLinkCreateProductPriceMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkCreateProductPriceMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CheckoutLinkCreateProductPriceMetadata$outboundSchema: z.ZodType<CheckoutLinkCreateProductPriceMetadata$Outbound, z.ZodTypeDef, CheckoutLinkCreateProductPriceMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkCreateProductPriceMetadata$ {
    /** @deprecated use `CheckoutLinkCreateProductPriceMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkCreateProductPriceMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkCreateProductPriceMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkCreateProductPriceMetadata$Outbound, z.ZodTypeDef, CheckoutLinkCreateProductPriceMetadata>;
    /** @deprecated use `CheckoutLinkCreateProductPriceMetadata$Outbound` instead. */
    type Outbound = CheckoutLinkCreateProductPriceMetadata$Outbound;
}
export declare function checkoutLinkCreateProductPriceMetadataToJSON(checkoutLinkCreateProductPriceMetadata: CheckoutLinkCreateProductPriceMetadata): string;
export declare function checkoutLinkCreateProductPriceMetadataFromJSON(jsonString: string): SafeParseResult<CheckoutLinkCreateProductPriceMetadata, SDKValidationError>;
/** @internal */
export declare const CheckoutLinkCreateProductPrice$inboundSchema: z.ZodType<CheckoutLinkCreateProductPrice, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkCreateProductPrice$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    payment_processor: "stripe";
    label?: string | null | undefined;
    allow_discount_codes: boolean;
    discount_id?: string | null | undefined;
    success_url?: string | null | undefined;
    product_price_id: string;
};
/** @internal */
export declare const CheckoutLinkCreateProductPrice$outboundSchema: z.ZodType<CheckoutLinkCreateProductPrice$Outbound, z.ZodTypeDef, CheckoutLinkCreateProductPrice>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkCreateProductPrice$ {
    /** @deprecated use `CheckoutLinkCreateProductPrice$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkCreateProductPrice, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkCreateProductPrice$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkCreateProductPrice$Outbound, z.ZodTypeDef, CheckoutLinkCreateProductPrice>;
    /** @deprecated use `CheckoutLinkCreateProductPrice$Outbound` instead. */
    type Outbound = CheckoutLinkCreateProductPrice$Outbound;
}
export declare function checkoutLinkCreateProductPriceToJSON(checkoutLinkCreateProductPrice: CheckoutLinkCreateProductPrice): string;
export declare function checkoutLinkCreateProductPriceFromJSON(jsonString: string): SafeParseResult<CheckoutLinkCreateProductPrice, SDKValidationError>;
//# sourceMappingURL=checkoutlinkcreateproductprice.d.ts.map