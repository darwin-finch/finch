import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitGitHubRepositoryProperties, BenefitGitHubRepositoryProperties$Outbound } from "./benefitgithubrepositoryproperties.js";
/**
 * A benefit of type `github_repository`.
 *
 * @remarks
 *
 * Use it to automatically invite your backers to a private GitHub repository.
 */
export type BenefitGitHubRepository = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the benefit.
     */
    id: string;
    type?: "github_repository" | undefined;
    /**
     * The description of the benefit.
     */
    description: string;
    /**
     * Whether the benefit is selectable when creating a product.
     */
    selectable: boolean;
    /**
     * Whether the benefit is deletable.
     */
    deletable: boolean;
    /**
     * The ID of the organization owning the benefit.
     */
    organizationId: string;
    /**
     * Properties for a benefit of type `github_repository`.
     */
    properties: BenefitGitHubRepositoryProperties;
};
/** @internal */
export declare const BenefitGitHubRepository$inboundSchema: z.ZodType<BenefitGitHubRepository, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitGitHubRepository$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    type: "github_repository";
    description: string;
    selectable: boolean;
    deletable: boolean;
    organization_id: string;
    properties: BenefitGitHubRepositoryProperties$Outbound;
};
/** @internal */
export declare const BenefitGitHubRepository$outboundSchema: z.ZodType<BenefitGitHubRepository$Outbound, z.ZodTypeDef, BenefitGitHubRepository>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitGitHubRepository$ {
    /** @deprecated use `BenefitGitHubRepository$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitGitHubRepository, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitGitHubRepository$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitGitHubRepository$Outbound, z.ZodTypeDef, BenefitGitHubRepository>;
    /** @deprecated use `BenefitGitHubRepository$Outbound` instead. */
    type Outbound = BenefitGitHubRepository$Outbound;
}
export declare function benefitGitHubRepositoryToJSON(benefitGitHubRepository: BenefitGitHubRepository): string;
export declare function benefitGitHubRepositoryFromJSON(jsonString: string): SafeParseResult<BenefitGitHubRepository, SDKValidationError>;
//# sourceMappingURL=benefitgithubrepository.d.ts.map