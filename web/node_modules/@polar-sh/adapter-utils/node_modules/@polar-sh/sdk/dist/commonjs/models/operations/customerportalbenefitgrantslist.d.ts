import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { BenefitType } from "../components/benefittype.js";
import { CustomerBenefitGrantSortProperty } from "../components/customerbenefitgrantsortproperty.js";
import { ListResourceCustomerBenefitGrant, ListResourceCustomerBenefitGrant$Outbound } from "../components/listresourcecustomerbenefitgrant.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalBenefitGrantsListSecurity = {
    customerSession: string;
};
/**
 * Filter by benefit type.
 */
export type QueryParamBenefitTypeFilter = BenefitType | Array<BenefitType>;
/**
 * Filter by benefit ID.
 */
export type CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter = string | Array<string>;
/**
 * Filter by organization ID.
 */
export type CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter = string | Array<string>;
/**
 * Filter by checkout ID.
 */
export type QueryParamCheckoutIDFilter = string | Array<string>;
/**
 * Filter by order ID.
 */
export type QueryParamOrderIDFilter = string | Array<string>;
/**
 * Filter by subscription ID.
 */
export type QueryParamSubscriptionIDFilter = string | Array<string>;
export type CustomerPortalBenefitGrantsListRequest = {
    /**
     * Filter by benefit type.
     */
    typeFilter?: BenefitType | Array<BenefitType> | null | undefined;
    /**
     * Filter by benefit ID.
     */
    benefitId?: string | Array<string> | null | undefined;
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by checkout ID.
     */
    checkoutId?: string | Array<string> | null | undefined;
    /**
     * Filter by order ID.
     */
    orderId?: string | Array<string> | null | undefined;
    /**
     * Filter by subscription ID.
     */
    subscriptionId?: string | Array<string> | null | undefined;
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
    sorting?: Array<CustomerBenefitGrantSortProperty> | null | undefined;
};
export type CustomerPortalBenefitGrantsListResponse = {
    result: ListResourceCustomerBenefitGrant;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsListSecurity$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsListSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsListSecurity$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListSecurity$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsListSecurity$ {
    /** @deprecated use `CustomerPortalBenefitGrantsListSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsListSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListSecurity$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListSecurity>;
    /** @deprecated use `CustomerPortalBenefitGrantsListSecurity$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsListSecurity$Outbound;
}
export declare function customerPortalBenefitGrantsListSecurityToJSON(customerPortalBenefitGrantsListSecurity: CustomerPortalBenefitGrantsListSecurity): string;
export declare function customerPortalBenefitGrantsListSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsListSecurity, SDKValidationError>;
/** @internal */
export declare const QueryParamBenefitTypeFilter$inboundSchema: z.ZodType<QueryParamBenefitTypeFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type QueryParamBenefitTypeFilter$Outbound = string | Array<string>;
/** @internal */
export declare const QueryParamBenefitTypeFilter$outboundSchema: z.ZodType<QueryParamBenefitTypeFilter$Outbound, z.ZodTypeDef, QueryParamBenefitTypeFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace QueryParamBenefitTypeFilter$ {
    /** @deprecated use `QueryParamBenefitTypeFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<QueryParamBenefitTypeFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `QueryParamBenefitTypeFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<QueryParamBenefitTypeFilter$Outbound, z.ZodTypeDef, QueryParamBenefitTypeFilter>;
    /** @deprecated use `QueryParamBenefitTypeFilter$Outbound` instead. */
    type Outbound = QueryParamBenefitTypeFilter$Outbound;
}
export declare function queryParamBenefitTypeFilterToJSON(queryParamBenefitTypeFilter: QueryParamBenefitTypeFilter): string;
export declare function queryParamBenefitTypeFilterFromJSON(jsonString: string): SafeParseResult<QueryParamBenefitTypeFilter, SDKValidationError>;
/** @internal */
export declare const CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$ {
    /** @deprecated use `CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter>;
    /** @deprecated use `CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter$Outbound;
}
export declare function customerPortalBenefitGrantsListQueryParamBenefitIDFilterToJSON(customerPortalBenefitGrantsListQueryParamBenefitIDFilter: CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter): string;
export declare function customerPortalBenefitGrantsListQueryParamBenefitIDFilterFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsListQueryParamBenefitIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter>;
    /** @deprecated use `CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter$Outbound;
}
export declare function customerPortalBenefitGrantsListQueryParamOrganizationIDFilterToJSON(customerPortalBenefitGrantsListQueryParamOrganizationIDFilter: CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter): string;
export declare function customerPortalBenefitGrantsListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const QueryParamCheckoutIDFilter$inboundSchema: z.ZodType<QueryParamCheckoutIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type QueryParamCheckoutIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const QueryParamCheckoutIDFilter$outboundSchema: z.ZodType<QueryParamCheckoutIDFilter$Outbound, z.ZodTypeDef, QueryParamCheckoutIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace QueryParamCheckoutIDFilter$ {
    /** @deprecated use `QueryParamCheckoutIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<QueryParamCheckoutIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `QueryParamCheckoutIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<QueryParamCheckoutIDFilter$Outbound, z.ZodTypeDef, QueryParamCheckoutIDFilter>;
    /** @deprecated use `QueryParamCheckoutIDFilter$Outbound` instead. */
    type Outbound = QueryParamCheckoutIDFilter$Outbound;
}
export declare function queryParamCheckoutIDFilterToJSON(queryParamCheckoutIDFilter: QueryParamCheckoutIDFilter): string;
export declare function queryParamCheckoutIDFilterFromJSON(jsonString: string): SafeParseResult<QueryParamCheckoutIDFilter, SDKValidationError>;
/** @internal */
export declare const QueryParamOrderIDFilter$inboundSchema: z.ZodType<QueryParamOrderIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type QueryParamOrderIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const QueryParamOrderIDFilter$outboundSchema: z.ZodType<QueryParamOrderIDFilter$Outbound, z.ZodTypeDef, QueryParamOrderIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace QueryParamOrderIDFilter$ {
    /** @deprecated use `QueryParamOrderIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<QueryParamOrderIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `QueryParamOrderIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<QueryParamOrderIDFilter$Outbound, z.ZodTypeDef, QueryParamOrderIDFilter>;
    /** @deprecated use `QueryParamOrderIDFilter$Outbound` instead. */
    type Outbound = QueryParamOrderIDFilter$Outbound;
}
export declare function queryParamOrderIDFilterToJSON(queryParamOrderIDFilter: QueryParamOrderIDFilter): string;
export declare function queryParamOrderIDFilterFromJSON(jsonString: string): SafeParseResult<QueryParamOrderIDFilter, SDKValidationError>;
/** @internal */
export declare const QueryParamSubscriptionIDFilter$inboundSchema: z.ZodType<QueryParamSubscriptionIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type QueryParamSubscriptionIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const QueryParamSubscriptionIDFilter$outboundSchema: z.ZodType<QueryParamSubscriptionIDFilter$Outbound, z.ZodTypeDef, QueryParamSubscriptionIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace QueryParamSubscriptionIDFilter$ {
    /** @deprecated use `QueryParamSubscriptionIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<QueryParamSubscriptionIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `QueryParamSubscriptionIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<QueryParamSubscriptionIDFilter$Outbound, z.ZodTypeDef, QueryParamSubscriptionIDFilter>;
    /** @deprecated use `QueryParamSubscriptionIDFilter$Outbound` instead. */
    type Outbound = QueryParamSubscriptionIDFilter$Outbound;
}
export declare function queryParamSubscriptionIDFilterToJSON(queryParamSubscriptionIDFilter: QueryParamSubscriptionIDFilter): string;
export declare function queryParamSubscriptionIDFilterFromJSON(jsonString: string): SafeParseResult<QueryParamSubscriptionIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomerPortalBenefitGrantsListRequest$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsListRequest$Outbound = {
    type_filter?: string | Array<string> | null | undefined;
    benefit_id?: string | Array<string> | null | undefined;
    organization_id?: string | Array<string> | null | undefined;
    checkout_id?: string | Array<string> | null | undefined;
    order_id?: string | Array<string> | null | undefined;
    subscription_id?: string | Array<string> | null | undefined;
    page: number;
    limit: number;
    sorting?: Array<string> | null | undefined;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsListRequest$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListRequest$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsListRequest$ {
    /** @deprecated use `CustomerPortalBenefitGrantsListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListRequest$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListRequest>;
    /** @deprecated use `CustomerPortalBenefitGrantsListRequest$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsListRequest$Outbound;
}
export declare function customerPortalBenefitGrantsListRequestToJSON(customerPortalBenefitGrantsListRequest: CustomerPortalBenefitGrantsListRequest): string;
export declare function customerPortalBenefitGrantsListRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsListRequest, SDKValidationError>;
/** @internal */
export declare const CustomerPortalBenefitGrantsListResponse$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsListResponse$Outbound = {
    Result: ListResourceCustomerBenefitGrant$Outbound;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsListResponse$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListResponse$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsListResponse$ {
    /** @deprecated use `CustomerPortalBenefitGrantsListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsListResponse$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsListResponse>;
    /** @deprecated use `CustomerPortalBenefitGrantsListResponse$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsListResponse$Outbound;
}
export declare function customerPortalBenefitGrantsListResponseToJSON(customerPortalBenefitGrantsListResponse: CustomerPortalBenefitGrantsListResponse): string;
export declare function customerPortalBenefitGrantsListResponseFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsListResponse, SDKValidationError>;
//# sourceMappingURL=customerportalbenefitgrantslist.d.ts.map