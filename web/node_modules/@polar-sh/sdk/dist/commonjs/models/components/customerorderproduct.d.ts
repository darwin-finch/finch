import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitBase, BenefitBase$Outbound } from "./benefitbase.js";
import { LegacyRecurringProductPrice, LegacyRecurringProductPrice$Outbound } from "./legacyrecurringproductprice.js";
import { Organization, Organization$Outbound } from "./organization.js";
import { ProductMediaFileRead, ProductMediaFileRead$Outbound } from "./productmediafileread.js";
import { ProductPrice, ProductPrice$Outbound } from "./productprice.js";
import { SubscriptionRecurringInterval } from "./subscriptionrecurringinterval.js";
export type CustomerOrderProductPrices = LegacyRecurringProductPrice | ProductPrice;
export type CustomerOrderProduct = {
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
    organization: Organization;
};
/** @internal */
export declare const CustomerOrderProductPrices$inboundSchema: z.ZodType<CustomerOrderProductPrices, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerOrderProductPrices$Outbound = LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
/** @internal */
export declare const CustomerOrderProductPrices$outboundSchema: z.ZodType<CustomerOrderProductPrices$Outbound, z.ZodTypeDef, CustomerOrderProductPrices>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerOrderProductPrices$ {
    /** @deprecated use `CustomerOrderProductPrices$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerOrderProductPrices, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerOrderProductPrices$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerOrderProductPrices$Outbound, z.ZodTypeDef, CustomerOrderProductPrices>;
    /** @deprecated use `CustomerOrderProductPrices$Outbound` instead. */
    type Outbound = CustomerOrderProductPrices$Outbound;
}
export declare function customerOrderProductPricesToJSON(customerOrderProductPrices: CustomerOrderProductPrices): string;
export declare function customerOrderProductPricesFromJSON(jsonString: string): SafeParseResult<CustomerOrderProductPrices, SDKValidationError>;
/** @internal */
export declare const CustomerOrderProduct$inboundSchema: z.ZodType<CustomerOrderProduct, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerOrderProduct$Outbound = {
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
    organization: Organization$Outbound;
};
/** @internal */
export declare const CustomerOrderProduct$outboundSchema: z.ZodType<CustomerOrderProduct$Outbound, z.ZodTypeDef, CustomerOrderProduct>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerOrderProduct$ {
    /** @deprecated use `CustomerOrderProduct$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerOrderProduct, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerOrderProduct$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerOrderProduct$Outbound, z.ZodTypeDef, CustomerOrderProduct>;
    /** @deprecated use `CustomerOrderProduct$Outbound` instead. */
    type Outbound = CustomerOrderProduct$Outbound;
}
export declare function customerOrderProductToJSON(customerOrderProduct: CustomerOrderProduct): string;
export declare function customerOrderProductFromJSON(jsonString: string): SafeParseResult<CustomerOrderProduct, SDKValidationError>;
//# sourceMappingURL=customerorderproduct.d.ts.map