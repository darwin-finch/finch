import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CustomerSubscriptionSortProperty } from "../components/customersubscriptionsortproperty.js";
import { ListResourceCustomerSubscription, ListResourceCustomerSubscription$Outbound } from "../components/listresourcecustomersubscription.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalSubscriptionsListSecurity = {
    customerSession: string;
};
/**
 * Filter by organization ID.
 */
export type CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter = string | Array<string>;
/**
 * Filter by product ID.
 */
export type CustomerPortalSubscriptionsListQueryParamProductIDFilter = string | Array<string>;
export type CustomerPortalSubscriptionsListRequest = {
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by product ID.
     */
    productId?: string | Array<string> | null | undefined;
    /**
     * Filter by active or cancelled subscription.
     */
    active?: boolean | null | undefined;
    /**
     * Search by product or organization name.
     */
    query?: string | null | undefined;
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
    sorting?: Array<CustomerSubscriptionSortProperty> | null | undefined;
};
export type CustomerPortalSubscriptionsListResponse = {
    result: ListResourceCustomerSubscription;
};
/** @internal */
export declare const CustomerPortalSubscriptionsListSecurity$inboundSchema: z.ZodType<CustomerPortalSubscriptionsListSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsListSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalSubscriptionsListSecurity$outboundSchema: z.ZodType<CustomerPortalSubscriptionsListSecurity$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsListSecurity$ {
    /** @deprecated use `CustomerPortalSubscriptionsListSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsListSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsListSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsListSecurity$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListSecurity>;
    /** @deprecated use `CustomerPortalSubscriptionsListSecurity$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsListSecurity$Outbound;
}
export declare function customerPortalSubscriptionsListSecurityToJSON(customerPortalSubscriptionsListSecurity: CustomerPortalSubscriptionsListSecurity): string;
export declare function customerPortalSubscriptionsListSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsListSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter>;
    /** @deprecated use `CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter$Outbound;
}
export declare function customerPortalSubscriptionsListQueryParamOrganizationIDFilterToJSON(customerPortalSubscriptionsListQueryParamOrganizationIDFilter: CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter): string;
export declare function customerPortalSubscriptionsListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomerPortalSubscriptionsListQueryParamProductIDFilter$inboundSchema: z.ZodType<CustomerPortalSubscriptionsListQueryParamProductIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsListQueryParamProductIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomerPortalSubscriptionsListQueryParamProductIDFilter$outboundSchema: z.ZodType<CustomerPortalSubscriptionsListQueryParamProductIDFilter$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListQueryParamProductIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsListQueryParamProductIDFilter$ {
    /** @deprecated use `CustomerPortalSubscriptionsListQueryParamProductIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsListQueryParamProductIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsListQueryParamProductIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsListQueryParamProductIDFilter$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListQueryParamProductIDFilter>;
    /** @deprecated use `CustomerPortalSubscriptionsListQueryParamProductIDFilter$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsListQueryParamProductIDFilter$Outbound;
}
export declare function customerPortalSubscriptionsListQueryParamProductIDFilterToJSON(customerPortalSubscriptionsListQueryParamProductIDFilter: CustomerPortalSubscriptionsListQueryParamProductIDFilter): string;
export declare function customerPortalSubscriptionsListQueryParamProductIDFilterFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsListQueryParamProductIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomerPortalSubscriptionsListRequest$inboundSchema: z.ZodType<CustomerPortalSubscriptionsListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsListRequest$Outbound = {
    organization_id?: string | Array<string> | null | undefined;
    product_id?: string | Array<string> | null | undefined;
    active?: boolean | null | undefined;
    query?: string | null | undefined;
    page: number;
    limit: number;
    sorting?: Array<string> | null | undefined;
};
/** @internal */
export declare const CustomerPortalSubscriptionsListRequest$outboundSchema: z.ZodType<CustomerPortalSubscriptionsListRequest$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsListRequest$ {
    /** @deprecated use `CustomerPortalSubscriptionsListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsListRequest$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListRequest>;
    /** @deprecated use `CustomerPortalSubscriptionsListRequest$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsListRequest$Outbound;
}
export declare function customerPortalSubscriptionsListRequestToJSON(customerPortalSubscriptionsListRequest: CustomerPortalSubscriptionsListRequest): string;
export declare function customerPortalSubscriptionsListRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsListRequest, SDKValidationError>;
/** @internal */
export declare const CustomerPortalSubscriptionsListResponse$inboundSchema: z.ZodType<CustomerPortalSubscriptionsListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsListResponse$Outbound = {
    Result: ListResourceCustomerSubscription$Outbound;
};
/** @internal */
export declare const CustomerPortalSubscriptionsListResponse$outboundSchema: z.ZodType<CustomerPortalSubscriptionsListResponse$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsListResponse$ {
    /** @deprecated use `CustomerPortalSubscriptionsListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsListResponse$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsListResponse>;
    /** @deprecated use `CustomerPortalSubscriptionsListResponse$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsListResponse$Outbound;
}
export declare function customerPortalSubscriptionsListResponseToJSON(customerPortalSubscriptionsListResponse: CustomerPortalSubscriptionsListResponse): string;
export declare function customerPortalSubscriptionsListResponseFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsListResponse, SDKValidationError>;
//# sourceMappingURL=customerportalsubscriptionslist.d.ts.map