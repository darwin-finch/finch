import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitBase, BenefitBase$Outbound } from "./benefitbase.js";
import { LegacyRecurringProductPrice, LegacyRecurringProductPrice$Outbound } from "./legacyrecurringproductprice.js";
import { ProductMediaFileRead, ProductMediaFileRead$Outbound } from "./productmediafileread.js";
import { ProductPrice, ProductPrice$Outbound } from "./productprice.js";
import { SubscriptionRecurringInterval } from "./subscriptionrecurringinterval.js";
export type CheckoutLinkProductPrices = LegacyRecurringProductPrice | ProductPrice;
/**
 * Product data for a checkout link.
 */
export type CheckoutLinkProduct = {
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
export declare const CheckoutLinkProductPrices$inboundSchema: z.ZodType<CheckoutLinkProductPrices, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkProductPrices$Outbound = LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
/** @internal */
export declare const CheckoutLinkProductPrices$outboundSchema: z.ZodType<CheckoutLinkProductPrices$Outbound, z.ZodTypeDef, CheckoutLinkProductPrices>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkProductPrices$ {
    /** @deprecated use `CheckoutLinkProductPrices$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkProductPrices, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkProductPrices$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkProductPrices$Outbound, z.ZodTypeDef, CheckoutLinkProductPrices>;
    /** @deprecated use `CheckoutLinkProductPrices$Outbound` instead. */
    type Outbound = CheckoutLinkProductPrices$Outbound;
}
export declare function checkoutLinkProductPricesToJSON(checkoutLinkProductPrices: CheckoutLinkProductPrices): string;
export declare function checkoutLinkProductPricesFromJSON(jsonString: string): SafeParseResult<CheckoutLinkProductPrices, SDKValidationError>;
/** @internal */
export declare const CheckoutLinkProduct$inboundSchema: z.ZodType<CheckoutLinkProduct, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkProduct$Outbound = {
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
export declare const CheckoutLinkProduct$outboundSchema: z.ZodType<CheckoutLinkProduct$Outbound, z.ZodTypeDef, CheckoutLinkProduct>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkProduct$ {
    /** @deprecated use `CheckoutLinkProduct$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkProduct, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkProduct$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkProduct$Outbound, z.ZodTypeDef, CheckoutLinkProduct>;
    /** @deprecated use `CheckoutLinkProduct$Outbound` instead. */
    type Outbound = CheckoutLinkProduct$Outbound;
}
export declare function checkoutLinkProductToJSON(checkoutLinkProduct: CheckoutLinkProduct): string;
export declare function checkoutLinkProductFromJSON(jsonString: string): SafeParseResult<CheckoutLinkProduct, SDKValidationError>;
//# sourceMappingURL=checkoutlinkproduct.d.ts.map