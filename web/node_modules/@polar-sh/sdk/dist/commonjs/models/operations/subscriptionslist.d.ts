import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ListResourceSubscription, ListResourceSubscription$Outbound } from "../components/listresourcesubscription.js";
import { SubscriptionSortProperty } from "../components/subscriptionsortproperty.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by organization ID.
 */
export type SubscriptionsListQueryParamOrganizationIDFilter = string | Array<string>;
/**
 * Filter by product ID.
 */
export type ProductIDFilter = string | Array<string>;
/**
 * Filter by customer ID.
 */
export type CustomerIDFilter = string | Array<string>;
/**
 * Filter by discount ID.
 */
export type DiscountIDFilter = string | Array<string>;
export type SubscriptionsListRequest = {
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by product ID.
     */
    productId?: string | Array<string> | null | undefined;
    /**
     * Filter by customer ID.
     */
    customerId?: string | Array<string> | null | undefined;
    /**
     * Filter by discount ID.
     */
    discountId?: string | Array<string> | null | undefined;
    /**
     * Filter by active or inactive subscription.
     */
    active?: boolean | null | undefined;
    /**
     * Page number, defaults to 1.
     */
    page?: number | undefined;
    /**
     * Size of a page, defaults to 10. Maximum is 100.
     */
    limit?: number | undefined;
    /**
     * Sorting criterion. Several criteria can be used simultaneously and will be applied in order. Add a minus sign `-` before the criteria name to sort by descending order.
     */
    sorting?: Array<SubscriptionSortProperty> | null | undefined;
};
export type SubscriptionsListResponse = {
    result: ListResourceSubscription;
};
/** @internal */
export declare const SubscriptionsListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<SubscriptionsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionsListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const SubscriptionsListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<SubscriptionsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, SubscriptionsListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionsListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `SubscriptionsListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionsListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, SubscriptionsListQueryParamOrganizationIDFilter>;
    /** @deprecated use `SubscriptionsListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = SubscriptionsListQueryParamOrganizationIDFilter$Outbound;
}
export declare function subscriptionsListQueryParamOrganizationIDFilterToJSON(subscriptionsListQueryParamOrganizationIDFilter: SubscriptionsListQueryParamOrganizationIDFilter): string;
export declare function subscriptionsListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<SubscriptionsListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const ProductIDFilter$inboundSchema: z.ZodType<ProductIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type ProductIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const ProductIDFilter$outboundSchema: z.ZodType<ProductIDFilter$Outbound, z.ZodTypeDef, ProductIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ProductIDFilter$ {
    /** @deprecated use `ProductIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ProductIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `ProductIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ProductIDFilter$Outbound, z.ZodTypeDef, ProductIDFilter>;
    /** @deprecated use `ProductIDFilter$Outbound` instead. */
    type Outbound = ProductIDFilter$Outbound;
}
export declare function productIDFilterToJSON(productIDFilter: ProductIDFilter): string;
export declare function productIDFilterFromJSON(jsonString: string): SafeParseResult<ProductIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomerIDFilter$inboundSchema: z.ZodType<CustomerIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomerIDFilter$outboundSchema: z.ZodType<CustomerIDFilter$Outbound, z.ZodTypeDef, CustomerIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerIDFilter$ {
    /** @deprecated use `CustomerIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerIDFilter$Outbound, z.ZodTypeDef, CustomerIDFilter>;
    /** @deprecated use `CustomerIDFilter$Outbound` instead. */
    type Outbound = CustomerIDFilter$Outbound;
}
export declare function customerIDFilterToJSON(customerIDFilter: CustomerIDFilter): string;
export declare function customerIDFilterFromJSON(jsonString: string): SafeParseResult<CustomerIDFilter, SDKValidationError>;
/** @internal */
export declare const DiscountIDFilter$inboundSchema: z.ZodType<DiscountIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type DiscountIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const DiscountIDFilter$outboundSchema: z.ZodType<DiscountIDFilter$Outbound, z.ZodTypeDef, DiscountIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace DiscountIDFilter$ {
    /** @deprecated use `DiscountIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<DiscountIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `DiscountIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<DiscountIDFilter$Outbound, z.ZodTypeDef, DiscountIDFilter>;
    /** @deprecated use `DiscountIDFilter$Outbound` instead. */
    type Outbound = DiscountIDFilter$Outbound;
}
export declare function discountIDFilterToJSON(discountIDFilter: DiscountIDFilter): string;
export declare function discountIDFilterFromJSON(jsonString: string): SafeParseResult<DiscountIDFilter, SDKValidationError>;
/** @internal */
export declare const SubscriptionsListRequest$inboundSchema: z.ZodType<SubscriptionsListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionsListRequest$Outbound = {
    organization_id?: string | Array<string> | null | undefined;
    product_id?: string | Array<string> | null | undefined;
    customer_id?: string | Array<string> | null | undefined;
    discount_id?: string | Array<string> | null | undefined;
    active?: boolean | null | undefined;
    page: number;
    limit: number;
    sorting?: Array<string> | null | undefined;
};
/** @internal */
export declare const SubscriptionsListRequest$outboundSchema: z.ZodType<SubscriptionsListRequest$Outbound, z.ZodTypeDef, SubscriptionsListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionsListRequest$ {
    /** @deprecated use `SubscriptionsListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionsListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionsListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionsListRequest$Outbound, z.ZodTypeDef, SubscriptionsListRequest>;
    /** @deprecated use `SubscriptionsListRequest$Outbound` instead. */
    type Outbound = SubscriptionsListRequest$Outbound;
}
export declare function subscriptionsListRequestToJSON(subscriptionsListRequest: SubscriptionsListRequest): string;
export declare function subscriptionsListRequestFromJSON(jsonString: string): SafeParseResult<SubscriptionsListRequest, SDKValidationError>;
/** @internal */
export declare const SubscriptionsListResponse$inboundSchema: z.ZodType<SubscriptionsListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionsListResponse$Outbound = {
    Result: ListResourceSubscription$Outbound;
};
/** @internal */
export declare const SubscriptionsListResponse$outboundSchema: z.ZodType<SubscriptionsListResponse$Outbound, z.ZodTypeDef, SubscriptionsListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionsListResponse$ {
    /** @deprecated use `SubscriptionsListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionsListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionsListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionsListResponse$Outbound, z.ZodTypeDef, SubscriptionsListResponse>;
    /** @deprecated use `SubscriptionsListResponse$Outbound` instead. */
    type Outbound = SubscriptionsListResponse$Outbound;
}
export declare function subscriptionsListResponseToJSON(subscriptionsListResponse: SubscriptionsListResponse): string;
export declare function subscriptionsListResponseFromJSON(jsonString: string): SafeParseResult<SubscriptionsListResponse, SDKValidationError>;
//# sourceMappingURL=subscriptionslist.d.ts.map