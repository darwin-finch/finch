import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitBase, BenefitBase$Outbound } from "./benefitbase.js";
import { LegacyRecurringProductPrice, LegacyRecurringProductPrice$Outbound } from "./legacyrecurringproductprice.js";
import { ProductMediaFileRead, ProductMediaFileRead$Outbound } from "./productmediafileread.js";
import { ProductPrice, ProductPrice$Outbound } from "./productprice.js";
import { SubscriptionRecurringInterval } from "./subscriptionrecurringinterval.js";
export type CheckoutProductPrices = LegacyRecurringProductPrice | ProductPrice;
/**
 * Product data for a checkout session.
 */
export type CheckoutProduct = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the product.
     */
    id: string;
    /**
     * The name of the product.
     */
    name: string;
    /**
     * The description of the product.
     */
    description: string | null;
    /**
     * The recurring interval of the product. If `None`, the product is a one-time purchase.
     */
    recurringInterval: SubscriptionRecurringInterval | null;
    /**
     * Whether the product is a subscription.
     */
    isRecurring: boolean;
    /**
     * Whether the product is archived and no longer available.
     */
    isArchived: boolean;
    /**
     * The ID of the organization owning the product.
     */
    organizationId: string;
    /**
     * List of prices for this product.
     */
    prices: Array<LegacyRecurringProductPrice | ProductPrice>;
    /**
     * List of benefits granted by the product.
     */
    benefits: Array<BenefitBase>;
    /**
     * List of medias associated to the product.
     */
    medias: Array<ProductMediaFileRead>;
};
/** @internal */
export declare const CheckoutProductPrices$inboundSchema: z.ZodType<CheckoutProductPrices, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutProductPrices$Outbound = LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
/** @internal */
export declare const CheckoutProductPrices$outboundSchema: z.ZodType<CheckoutProductPrices$Outbound, z.ZodTypeDef, CheckoutProductPrices>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutProductPrices$ {
    /** @deprecated use `CheckoutProductPrices$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutProductPrices, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutProductPrices$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutProductPrices$Outbound, z.ZodTypeDef, CheckoutProductPrices>;
    /** @deprecated use `CheckoutProductPrices$Outbound` instead. */
    type Outbound = CheckoutProductPrices$Outbound;
}
export declare function checkoutProductPricesToJSON(checkoutProductPrices: CheckoutProductPrices): string;
export declare function checkoutProductPricesFromJSON(jsonString: string): SafeParseResult<CheckoutProductPrices, SDKValidationError>;
/** @internal */
export declare const CheckoutProduct$inboundSchema: z.ZodType<CheckoutProduct, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutProduct$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    name: string;
    description: string | null;
    recurring_interval: string | null;
    is_recurring: boolean;
    is_archived: boolean;
    organization_id: string;
    prices: Array<LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound>;
    benefits: Array<BenefitBase$Outbound>;
    medias: Array<ProductMediaFileRead$Outbound>;
};
/** @internal */
export declare const CheckoutProduct$outboundSchema: z.ZodType<CheckoutProduct$Outbound, z.ZodTypeDef, CheckoutProduct>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutProduct$ {
    /** @deprecated use `CheckoutProduct$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutProduct, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutProduct$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutProduct$Outbound, z.ZodTypeDef, CheckoutProduct>;
    /** @deprecated use `CheckoutProduct$Outbound` instead. */
    type Outbound = CheckoutProduct$Outbound;
}
export declare function checkoutProductToJSON(checkoutProduct: CheckoutProduct): string;
export declare function checkoutProductFromJSON(jsonString: string): SafeParseResult<CheckoutProduct, SDKValidationError>;
//# sourceMappingURL=checkoutproduct.d.ts.map