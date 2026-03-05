import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ListResourceRefund, ListResourceRefund$Outbound } from "../components/listresourcerefund.js";
import { RefundSortProperty } from "../components/refundsortproperty.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by refund ID.
 */
export type RefundIDFilter = string | Array<string>;
/**
 * Filter by organization ID.
 */
export type RefundsListQueryParamOrganizationIDFilter = string | Array<string>;
/**
 * Filter by order ID.
 */
export type OrderIDFilter = string | Array<string>;
/**
 * Filter by subscription ID.
 */
export type SubscriptionIDFilter = string | Array<string>;
/**
 * Filter by customer ID.
 */
export type RefundsListQueryParamCustomerIDFilter = string | Array<string>;
export type RefundsListRequest = {
    /**
     * Filter by refund ID.
     */
    id?: string | Array<string> | null | undefined;
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by order ID.
     */
    orderId?: string | Array<string> | null | undefined;
    /**
     * Filter by subscription ID.
     */
    subscriptionId?: string | Array<string> | null | undefined;
    /**
     * Filter by customer ID.
     */
    customerId?: string | Array<string> | null | undefined;
    /**
     * Filter by `succeeded`.
     */
    succeeded?: boolean | null | undefined;
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
    sorting?: Array<RefundSortProperty> | null | undefined;
};
export type RefundsListResponse = {
    result: ListResourceRefund;
};
/** @internal */
export declare const RefundIDFilter$inboundSchema: z.ZodType<RefundIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type RefundIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const RefundIDFilter$outboundSchema: z.ZodType<RefundIDFilter$Outbound, z.ZodTypeDef, RefundIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundIDFilter$ {
    /** @deprecated use `RefundIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<RefundIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `RefundIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<RefundIDFilter$Outbound, z.ZodTypeDef, RefundIDFilter>;
    /** @deprecated use `RefundIDFilter$Outbound` instead. */
    type Outbound = RefundIDFilter$Outbound;
}
export declare function refundIDFilterToJSON(refundIDFilter: RefundIDFilter): string;
export declare function refundIDFilterFromJSON(jsonString: string): SafeParseResult<RefundIDFilter, SDKValidationError>;
/** @internal */
export declare const RefundsListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<RefundsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type RefundsListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const RefundsListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<RefundsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, RefundsListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundsListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `RefundsListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<RefundsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `RefundsListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<RefundsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, RefundsListQueryParamOrganizationIDFilter>;
    /** @deprecated use `RefundsListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = RefundsListQueryParamOrganizationIDFilter$Outbound;
}
export declare function refundsListQueryParamOrganizationIDFilterToJSON(refundsListQueryParamOrganizationIDFilter: RefundsListQueryParamOrganizationIDFilter): string;
export declare function refundsListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<RefundsListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const OrderIDFilter$inboundSchema: z.ZodType<OrderIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type OrderIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const OrderIDFilter$outboundSchema: z.ZodType<OrderIDFilter$Outbound, z.ZodTypeDef, OrderIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace OrderIDFilter$ {
    /** @deprecated use `OrderIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<OrderIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `OrderIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<OrderIDFilter$Outbound, z.ZodTypeDef, OrderIDFilter>;
    /** @deprecated use `OrderIDFilter$Outbound` instead. */
    type Outbound = OrderIDFilter$Outbound;
}
export declare function orderIDFilterToJSON(orderIDFilter: OrderIDFilter): string;
export declare function orderIDFilterFromJSON(jsonString: string): SafeParseResult<OrderIDFilter, SDKValidationError>;
/** @internal */
export declare const SubscriptionIDFilter$inboundSchema: z.ZodType<SubscriptionIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const SubscriptionIDFilter$outboundSchema: z.ZodType<SubscriptionIDFilter$Outbound, z.ZodTypeDef, SubscriptionIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionIDFilter$ {
    /** @deprecated use `SubscriptionIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionIDFilter$Outbound, z.ZodTypeDef, SubscriptionIDFilter>;
    /** @deprecated use `SubscriptionIDFilter$Outbound` instead. */
    type Outbound = SubscriptionIDFilter$Outbound;
}
export declare function subscriptionIDFilterToJSON(subscriptionIDFilter: SubscriptionIDFilter): string;
export declare function subscriptionIDFilterFromJSON(jsonString: string): SafeParseResult<SubscriptionIDFilter, SDKValidationError>;
/** @internal */
export declare const RefundsListQueryParamCustomerIDFilter$inboundSchema: z.ZodType<RefundsListQueryParamCustomerIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type RefundsListQueryParamCustomerIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const RefundsListQueryParamCustomerIDFilter$outboundSchema: z.ZodType<RefundsListQueryParamCustomerIDFilter$Outbound, z.ZodTypeDef, RefundsListQueryParamCustomerIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundsListQueryParamCustomerIDFilter$ {
    /** @deprecated use `RefundsListQueryParamCustomerIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<RefundsListQueryParamCustomerIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `RefundsListQueryParamCustomerIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<RefundsListQueryParamCustomerIDFilter$Outbound, z.ZodTypeDef, RefundsListQueryParamCustomerIDFilter>;
    /** @deprecated use `RefundsListQueryParamCustomerIDFilter$Outbound` instead. */
    type Outbound = RefundsListQueryParamCustomerIDFilter$Outbound;
}
export declare function refundsListQueryParamCustomerIDFilterToJSON(refundsListQueryParamCustomerIDFilter: RefundsListQueryParamCustomerIDFilter): string;
export declare function refundsListQueryParamCustomerIDFilterFromJSON(jsonString: string): SafeParseResult<RefundsListQueryParamCustomerIDFilter, SDKValidationError>;
/** @internal */
export declare const RefundsListRequest$inboundSchema: z.ZodType<RefundsListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type RefundsListRequest$Outbound = {
    id?: string | Array<string> | null | undefined;
    organization_id?: string | Array<string> | null | undefined;
    order_id?: string | Array<string> | null | undefined;
    subscription_id?: string | Array<string> | null | undefined;
    customer_id?: string | Array<string> | null | undefined;
    succeeded?: boolean | null | undefined;
    page: number;
    limit: number;
    sorting?: Array<string> | null | undefined;
};
/** @internal */
export declare const RefundsListRequest$outboundSchema: z.ZodType<RefundsListRequest$Outbound, z.ZodTypeDef, RefundsListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundsListRequest$ {
    /** @deprecated use `RefundsListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<RefundsListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `RefundsListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<RefundsListRequest$Outbound, z.ZodTypeDef, RefundsListRequest>;
    /** @deprecated use `RefundsListRequest$Outbound` instead. */
    type Outbound = RefundsListRequest$Outbound;
}
export declare function refundsListRequestToJSON(refundsListRequest: RefundsListRequest): string;
export declare function refundsListRequestFromJSON(jsonString: string): SafeParseResult<RefundsListRequest, SDKValidationError>;
/** @internal */
export declare const RefundsListResponse$inboundSchema: z.ZodType<RefundsListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type RefundsListResponse$Outbound = {
    Result: ListResourceRefund$Outbound;
};
/** @internal */
export declare const RefundsListResponse$outboundSchema: z.ZodType<RefundsListResponse$Outbound, z.ZodTypeDef, RefundsListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundsListResponse$ {
    /** @deprecated use `RefundsListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<RefundsListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `RefundsListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<RefundsListResponse$Outbound, z.ZodTypeDef, RefundsListResponse>;
    /** @deprecated use `RefundsListResponse$Outbound` instead. */
    type Outbound = RefundsListResponse$Outbound;
}
export declare function refundsListResponseToJSON(refundsListResponse: RefundsListResponse): string;
export declare function refundsListResponseFromJSON(jsonString: string): SafeParseResult<RefundsListResponse, SDKValidationError>;
//# sourceMappingURL=refundslist.d.ts.map